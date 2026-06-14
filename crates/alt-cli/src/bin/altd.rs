//! `altd`: the per-repository local daemon. It holds one open [`Store`] and
//! serves commands over a Unix socket (`<alt-dir>/daemon.sock`) so each `alt`
//! invocation skips the ~21ms per-command open (mmap packs, read `map.alt`,
//! replay the op log). It is a transparent performance cache: coherence with
//! direct `alt` invocations and external git tooling is kept by refreshing the
//! store at the start of every request (the same concurrency machinery the
//! direct CLI uses — the daemon is just another reader/writer).
//!
//! Concurrency (D5a): a multi-threaded accept loop. Each connection is served
//! on its own thread, but the held `(Store, Repository)` is wrapped in one
//! `Mutex`, so request *execution* is serialized while connection accept and
//! response write-back run concurrently (a slow request never blocks accepting
//! the next connection). The in-process `Mutex` is load-bearing: `flock` does
//! not make threads of one process mutually exclusive (the lock lives on the
//! shared open file description), so the daemon cannot lean on the S4–S10 file
//! locks to serialize its own threads — only across processes.
//!
//! Group commit (D5b): a write appends under the store `Mutex` but defers its
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
    use std::sync::{Arc, Condvar, Mutex};

    use alt_cli::cli::{self, Cli};
    use alt_cli::daemon::{self, Request, Response};
    use alt_cli::native::{Identity, Store, StoreSink};
    use alt_repo::Repository;
    use clap::Parser;

    type Res<T> = Result<T, Box<dyn std::error::Error>>;

    /// The daemon's shared state: one open store + git-layer repository behind a
    /// single `Mutex`. One lock for both avoids any lock-ordering question and
    /// matches how a request uses them together (refresh both, then dispatch).
    type Shared = Arc<Mutex<(Store, Repository)>>;

    /// Exit when no request arrives within this window (overridable via
    /// `ALT_DAEMON_IDLE_MS`, mainly for tests).
    const DEFAULT_IDLE_MS: i32 = 10 * 60 * 1000;

    /// In-process group commit (D5b). A write appends under the store `Mutex`
    /// but defers its fsync; the actual fsync is the slow part and, if every
    /// request fsynced inline under the store `Mutex`, commits would be fully
    /// serialized (one fsync each) and concurrency would buy nothing. Instead a
    /// committer assigns itself a `ticket` (a monotonic seq, handed out while it
    /// holds the store `Mutex` right after appending) and then, in a separate
    /// step, waits for *some* committer to perform one fsync that covers it.
    ///
    /// One fsync covers everyone: [`StoreSink::fsync_all`] flushes to the true
    /// on-disk EOF, so a single fsync makes durable every append already on
    /// disk. The leader snapshots `covered = next_ticket`, fsyncs off the store
    /// `Mutex` (so appends overlap it), and publishes `durable = covered`;
    /// because a ticket's bytes are on disk before its ticket is handed out,
    /// every ticket `<= covered` is now durable. Followers whose ticket
    /// `<= durable` are done without fsyncing. So N concurrent commits coalesce
    /// onto ~1 fsync, the lever that lifts throughput past the
    /// one-fsync-per-commit ceiling.
    struct GroupCommit {
        inner: Mutex<GroupInner>,
        cv: Condvar,
    }

    struct GroupInner {
        /// Highest ticket handed out (each commit takes `next_ticket += 1`).
        next_ticket: u64,
        /// Highest ticket made durable by a completed fsync.
        durable: u64,
        /// A leader is currently fsyncing; others wait rather than pile on.
        syncing: bool,
        /// Error from the most recent fsync (clears on the next success), so a
        /// committer whose own flush failed reports it instead of looping.
        last_error: Option<String>,
    }

    impl GroupCommit {
        fn new() -> Self {
            GroupCommit {
                inner: Mutex::new(GroupInner {
                    next_ticket: 0,
                    durable: 0,
                    syncing: false,
                    last_error: None,
                }),
                cv: Condvar::new(),
            }
        }

        /// Hands out the next ticket. Called by a committer while it holds the
        /// store `Mutex` (right after appending), so tickets are in commit order
        /// and a ticket's bytes are already on disk.
        fn assign(&self) -> u64 {
            let mut g = self.inner.lock().expect("group mutex poisoned");
            g.next_ticket += 1;
            g.next_ticket
        }

        /// Blocks until `ticket` is durable, performing the fsync itself (via
        /// the off-write-path `sink`) if no other committer is. Leads at most
        /// once: if its own flush fails, it returns the error rather than
        /// spinning on a dead disk.
        ///
        /// The fsync runs **without** the store `Mutex`, so other requests keep
        /// appending while it runs — that overlap is what lets a single fsync
        /// cover many commits. A ticket's bytes are on disk before its ticket is
        /// handed out (both happen under the store `Mutex`), so a fsync that
        /// reads the inode after `covered = next_ticket` flushes every ticket
        /// `<= covered`.
        fn await_durable(&self, sink: &StoreSink, ticket: u64) -> Result<(), String> {
            let mut g = self.inner.lock().expect("group mutex poisoned");
            let mut led = false;
            loop {
                if g.durable >= ticket {
                    return Ok(());
                }
                if g.syncing {
                    g = self.cv.wait(g).expect("group mutex poisoned");
                    continue;
                }
                if led {
                    // we already fsynced once and are still uncovered → our
                    // flush failed; surface it rather than retry a dead disk
                    return Err(g
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "durability failed".to_owned()));
                }
                // become the leader for this batch
                g.syncing = true;
                led = true;
                let covered = g.next_ticket;
                drop(g);
                let outcome = sink
                    .fsync_all()
                    .map(|()| covered)
                    .map_err(|e| e.to_string());
                g = self.inner.lock().expect("group mutex poisoned");
                g.syncing = false;
                match outcome {
                    Ok(covered) => {
                        g.durable = g.durable.max(covered);
                        g.last_error = None;
                    }
                    Err(e) => g.last_error = Some(e),
                }
                self.cv.notify_all();
            }
        }
    }

    pub fn run() -> Res<()> {
        let alt_dir = std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .ok_or("usage: altd <alt-dir>")?;
        let sock_path = alt_dir.join("daemon.sock");

        let listener = bind(&sock_path)?;
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
            let mut fds = [libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };
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
        // phase 1: run the command. A write appends but defers its fsync; if the
        // write epoch advanced, take a ticket while still holding the store
        // Mutex (so the ticket is in commit order and its bytes are on disk).
        let (resp, ticket) = {
            let mut guard = shared.lock().expect("daemon store mutex poisoned");
            let (store, repo) = &mut *guard;
            let before = store.write_epoch();
            let resp = handle(store, repo, &req);
            let ticket = (store.write_epoch() != before).then(|| group.assign());
            (resp, ticket)
        };
        // phase 2: for a write, wait for the group-commit fsync that covers this
        // ticket — performing it ourselves if no one else is. The fsync runs off
        // the store Mutex, so concurrent commits append meanwhile and one fsync
        // covers them all. Reads carry no ticket and skip this entirely.
        let resp = match ticket {
            Some(t) => match group.await_durable(sink, t) {
                Ok(()) => resp,
                Err(e) => Response {
                    exit_code: 128,
                    stdout: Vec::new(),
                    stderr: format!("fatal: durability: {e}\n").into_bytes(),
                },
            },
            None => resp,
        };
        daemon::write_frame(&mut stream, &resp.encode())?;
        Ok(())
    }

    /// Runs a request, turning any error into a `fatal:` stderr + exit 128 — the
    /// same shape the `alt` binary reports for an uncaught error.
    fn handle(store: &mut Store, repo: &mut Repository, req: &Request) -> Response {
        match dispatch(store, repo, req) {
            Ok((exit_code, stdout)) => Response {
                exit_code,
                stdout,
                stderr: Vec::new(),
            },
            Err(e) => Response {
                exit_code: 128,
                stdout: Vec::new(),
                stderr: format!("fatal: {e}\n").into_bytes(),
            },
        }
    }

    fn dispatch(store: &mut Store, repo: &mut Repository, req: &Request) -> Res<(u8, Vec<u8>)> {
        // the request argv carries no program name; clap expects one
        let argv = std::iter::once("alt".to_owned()).chain(req.args.iter().cloned());
        let cli = Cli::try_parse_from(argv)?;
        let id = Identity::from_map(&req.env);
        // per-request catch-up on both the native store and the git-layer
        // repository: see writes other processes committed since the last
        // request, so a served read is never stale
        store.refresh()?;
        repo.refresh()?;
        let mut out = Vec::new();
        let code = cli::run_on_store(&cli, store, repo, &req.cwd, id, &mut out)?;
        Ok((code, out))
    }
}
