//! `altd`: the per-repository local daemon. It holds one open [`Store`] and
//! serves commands over a Unix socket (`<alt-dir>/daemon.sock`) so each `alt`
//! invocation skips the ~21ms per-command open (mmap packs, read `map.alt`,
//! replay the op log). It is a transparent performance cache: coherence with
//! direct `alt` invocations and external git tooling is kept by refreshing the
//! store at the start of every request (the same concurrency machinery the
//! direct CLI uses — the daemon is just another reader/writer).
//!
//! Concurrency: a multi-threaded accept loop. Each connection is served
//! on its own thread, but the held `(Store, Repository)` is wrapped in one
//! `Mutex`, so request *execution* is serialized while connection accept and
//! response write-back run concurrently (a slow request never blocks accepting
//! the next connection). The in-process `Mutex` is load-bearing: `flock` does
//! not make threads of one process mutually exclusive (the lock lives on the
//! shared open file description), so the daemon cannot lean on the store's file
//! locks to serialize its own threads — only across processes.
//!
//! Group commit: a write appends under the store `Mutex` but defers its
//! fsync; the slow fsync is then performed *off* the `Mutex` by a [`GroupCommit`]
//! coordinator (through independent fds, see [`StoreSink`]), so concurrent
//! commits keep appending while one fsync runs and coalesce onto it. That lifts
//! commit throughput past the one-fsync-per-commit ceiling.

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("altd is only supported on unix");
    std::process::ExitCode::from(1)
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    match unix::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("altd: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use alt_cli::daemon::{self, Request};
    use alt_cli::execution::Execution;
    use alt_cli::group_commit::GroupCommit;
    use alt_cli::machine::{self, Failure};
    use alt_cli::native::{Identity, Store, StoreSink};
    use alt_repo::Repository;

    type Res<T> = Result<T, Box<dyn std::error::Error>>;

    /// The daemon's shared state: one open store + git-layer repository behind a
    /// single `Mutex`. One lock for both avoids any lock-ordering question and
    /// matches how a request uses them together (refresh both, then dispatch).
    type Shared = Arc<Mutex<(Store, Repository)>>;

    /// Exit when no request arrives within this window (overridable via
    /// `ALT_DAEMON_IDLE_MS`, mainly for tests).
    const DEFAULT_IDLE_MS: i32 = 10 * 60 * 1000;

    pub fn run() -> Res<()> {
        let alt_dir = std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .ok_or("usage: altd <alt-dir>")?;
        let sock_path = alt_dir.join("daemon.sock");

        let listener = bind(&sock_path)?;
        // Drop a small metadata file alongside the socket so clients can
        // tell whether the daemon is still the same `altd` binary they
        // have on disk. The next `alt` invocation reads this, compares
        // against its own `altd`'s mtime + size, and kills+respawns us
        // when the binary moved on (cargo install path). Best-effort —
        // a write failure just disables the auto-reload, never the
        // daemon itself.
        let _ = write_meta(&alt_dir);
        // the held store serves native commands; the held repository serves
        // git-layer reads (`log`) — both opened once and refreshed per request,
        // and shared across request threads behind one Mutex
        let repo_root = alt_dir.parent().unwrap_or(&alt_dir).to_path_buf();
        let mut store = Store::open(alt_dir)?;
        // in-process group commit: a write appends under the per-request lock but
        // defers its fsync; `serve` coalesces the fsync in a separate, shorter
        // critical section, so N concurrent commits batch onto ~1 fsync
        store.set_defer_durability(true);
        // the group-commit coordinator's fsync handle — its own fds, so it
        // fsyncs without the store Mutex and appends overlap the fsync
        let sink = Arc::new(store.sink()?);
        let repo = Repository::discover(&repo_root)?;
        let shared: Shared = Arc::new(Mutex::new((store, repo)));
        let group = Arc::new(GroupCommit::new());

        let idle_ms = std::env::var("ALT_DAEMON_IDLE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_IDLE_MS);

        // count of requests accepted but not yet fully served. Incremented in
        // this thread before the worker spawns (so an immediate idle poll can't
        // race past an in-flight request), decremented when the worker is done.
        let active = Arc::new(AtomicUsize::new(0));
        let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();

        // serve until an idle window with no request in flight, then exit and
        // let the next client respawn us. A slow request runs on its own thread,
        // so it blocks neither accepting the next connection nor other requests'
        // I/O — only the shared store, via the Mutex inside `serve`.
        loop {
            match accept_with_timeout(&listener, idle_ms)? {
                Some(stream) => {
                    active.fetch_add(1, Ordering::SeqCst);
                    let shared = Arc::clone(&shared);
                    let group = Arc::clone(&group);
                    let sink = Arc::clone(&sink);
                    let active = Arc::clone(&active);
                    workers.push(std::thread::spawn(move || {
                        if let Err(e) = serve(&shared, &group, &sink, stream) {
                            eprintln!("altd: request error: {e}");
                        }
                        active.fetch_sub(1, Ordering::SeqCst);
                    }));
                    // reap finished workers so the handle vec stays bounded
                    workers.retain(|h| !h.is_finished());
                }
                None if active.load(Ordering::SeqCst) == 0 => break,
                None => {} // idle window elapsed but a request is still in flight
            }
        }
        // join any worker still finishing its write-back before we tear down
        for h in workers {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&sock_path);
        Ok(())
    }

    /// Records the running daemon's identity next to its socket: pid +
    /// `altd` path + the binary's mtime-nanos + size. Clients compare
    /// these against their own `altd` and kill+respawn on mismatch
    /// (the fix for "cargo install upgraded the binary but
    /// the old daemon still answers"). Best-effort: any io failure
    /// disables auto-reload without affecting the daemon's lifecycle.
    fn write_meta(alt_dir: &Path) -> Option<()> {
        let altd = std::env::current_exe().ok()?;
        let meta = std::fs::metadata(&altd).ok()?;
        let mtime_nanos = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let size = meta.len();
        let pid = std::process::id();
        let altd_str = altd.display().to_string();
        let content = format!("{pid}\n{altd_str}\n{mtime_nanos}\n{size}\n");
        let path = alt_dir.join("daemon.meta");
        // Write atomically via tmp + rename so a client can never read
        // a torn meta during the swap.
        let tmp = path.with_extension("meta.tmp");
        std::fs::write(&tmp, content).ok()?;
        std::fs::rename(&tmp, &path).ok()
    }

    /// Binds the listening socket, self-healing a stale socket file and bowing
    /// out if a live daemon already owns it.
    fn bind(sock_path: &Path) -> Res<UnixListener> {
        match UnixListener::bind(sock_path) {
            Ok(l) => Ok(l),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                // a socket file is in the way: a live daemon, or a stale file
                // left by a crashed one?
                if UnixStream::connect(sock_path).is_ok() {
                    return Err("a daemon is already running for this repository".into());
                }
                let _ = std::fs::remove_file(sock_path);
                Ok(UnixListener::bind(sock_path)?)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Blocks for the next connection up to `timeout_ms`, returning `None` on
    /// timeout. `poll` gives both full responsiveness and an idle deadline,
    /// where a blocking `accept` would give no deadline.
    fn accept_with_timeout(
        listener: &UnixListener,
        timeout_ms: i32,
    ) -> io::Result<Option<UnixStream>> {
        loop {
            let mut fds = [alt_os::sys::pollfd {
                fd: listener.as_raw_fd(),
                events: alt_os::sys::POLLIN,
                revents: 0,
            }];
            let rc = unsafe { alt_os::sys::poll(fds.as_mut_ptr(), 1, timeout_ms) };
            if rc < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue; // a signal interrupted the wait; keep waiting
                }
                return Err(e);
            }
            if rc == 0 {
                return Ok(None); // timed out idle
            }
            let (stream, _addr) = listener.accept()?;
            return Ok(Some(stream));
        }
    }

    /// Reads one request, runs it against the held store/repo, writes back.
    /// Framing I/O happens outside the lock; only `handle` (refresh + dispatch)
    /// holds it, so the serialized section is the store work, not the wire I/O.
    fn serve(
        shared: &Shared,
        group: &GroupCommit,
        sink: &StoreSink,
        mut stream: UnixStream,
    ) -> Res<()> {
        let payload = daemon::read_frame(&mut stream)?;
        let req = Request::decode(&payload)?;
        let parsed = match machine::parse(
            std::iter::once("alt".into())
                .chain(req.args.iter().map(Into::into))
                .collect(),
        ) {
            Ok(parsed) => parsed,
            Err(response) => {
                daemon::write_frame(&mut stream, &response.encode())?;
                return Ok(());
            }
        };
        let execution = match Execution::prepare(
            parsed,
            req.cwd.clone(),
            Identity::from_map(&req.env),
            req.id,
        ) {
            Ok(execution) => execution,
            Err(response) => {
                daemon::write_frame(&mut stream, &response.encode())?;
                return Ok(());
            }
        };
        let (response, operations, ticket, barrier_ok) = {
            let mut guard = shared.lock().expect("daemon store mutex poisoned");
            let (store, repo) = &mut *guard;
            let refreshed = store
                .refresh()
                .and_then(|()| repo.refresh().map_err(Into::into));
            let (mut response, operations) = match refreshed {
                Ok(()) => execution.on_store(store, repo),
                Err(error) => (execution.failure(error.as_ref()), Vec::new()),
            };
            let mut barrier_ok = true;
            if let Err(error) = store.odb_mut().flush_pending() {
                response = Failure::unknown(format!("durability: {error}")).response(
                    execution.parsed.json,
                    execution.id.as_ref().map(|id| machine::hex(id)).as_deref(),
                );
                barrier_ok = false;
            }
            let ticket = execution.writing.then(|| group.assign());
            (response, operations, ticket, barrier_ok)
        };
        let response = if barrier_ok {
            match ticket
                .map(|ticket| group.await_durable(sink, ticket))
                .transpose()
            {
                Ok(_) => execution.publish(response, operations),
                Err(error) => Failure::unknown(format!("durability: {error}")).response(
                    execution.parsed.json,
                    execution.id.as_ref().map(|id| machine::hex(id)).as_deref(),
                ),
            }
        } else {
            response
        };
        daemon::write_frame(&mut stream, &response.encode())?;
        Ok(())
    }
}
