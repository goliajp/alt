//! `altd`: the per-repository local daemon. It holds one open [`Store`] and
//! serves commands over a Unix socket (`<alt-dir>/daemon.sock`) so each `alt`
//! invocation skips the ~21ms per-command open (mmap packs, read `map.alt`,
//! replay the op log). It is a transparent performance cache: coherence with
//! direct `alt` invocations and external git tooling is kept by refreshing the
//! store at the start of every request (the same concurrency machinery the
//! direct CLI uses — the daemon is just another reader/writer).
//!
//! D5a scope: a multi-threaded accept loop. Each connection is served on its
//! own thread, but the held `(Store, Repository)` is wrapped in one `Mutex`, so
//! request *execution* is serialized while connection accept and response
//! write-back run concurrently (a slow request never blocks accepting the next
//! connection). The in-process `Mutex` is load-bearing: `flock` does not make
//! threads of one process mutually exclusive (the lock lives on the shared open
//! file description), so the daemon cannot lean on the S4–S10 file locks to
//! serialize its own threads — only across processes. Read-concurrency and
//! in-process group commit are later D5 steps; D5a is the concurrency skeleton.

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

    use alt_cli::cli::{self, Cli};
    use alt_cli::daemon::{self, Request, Response};
    use alt_cli::native::{Identity, Store};
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
        let store = Store::open(alt_dir)?;
        let repo = Repository::discover(&repo_root)?;
        let shared: Shared = Arc::new(Mutex::new((store, repo)));

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
                    let active = Arc::clone(&active);
                    workers.push(std::thread::spawn(move || {
                        if let Err(e) = serve(&shared, stream) {
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
    fn serve(shared: &Shared, mut stream: UnixStream) -> Res<()> {
        let payload = daemon::read_frame(&mut stream)?;
        let req = Request::decode(&payload)?;
        let resp = {
            let mut guard = shared.lock().expect("daemon store mutex poisoned");
            let (store, repo) = &mut *guard;
            handle(store, repo, &req)
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
