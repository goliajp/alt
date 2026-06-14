//! `altd`: the per-repository local daemon. It holds one open [`Store`] and
//! serves commands over a Unix socket (`<alt-dir>/daemon.sock`) so each `alt`
//! invocation skips the ~21ms per-command open (mmap packs, read `map.alt`,
//! replay the op log). It is a transparent performance cache: coherence with
//! direct `alt` invocations and external git tooling is kept by refreshing the
//! store at the start of every request (the same concurrency machinery the
//! direct CLI uses — the daemon is just another reader/writer).
//!
//! D2 scope: a single-threaded accept loop (one request at a time against the
//! held store). Concurrent in-process request handling is a later step; the
//! meaningful concurrency today is the daemon contending with *other processes*
//! on the store, which the file locks already serialize.

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

    use alt_cli::cli::{self, Cli};
    use alt_cli::daemon::{self, Request, Response};
    use alt_cli::native::{Identity, Store};
    use clap::Parser;

    type Res<T> = Result<T, Box<dyn std::error::Error>>;

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
        let mut store = Store::open(alt_dir)?;

        let idle_ms = std::env::var("ALT_DAEMON_IDLE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_IDLE_MS);

        // serve until an idle timeout (None) — then exit and let the next
        // client respawn us
        while let Some(stream) = accept_with_timeout(&listener, idle_ms)? {
            if let Err(e) = serve(&mut store, stream) {
                eprintln!("altd: request error: {e}");
            }
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

    /// Reads one request, runs it against the held store, writes the response.
    fn serve(store: &mut Store, mut stream: UnixStream) -> Res<()> {
        let payload = daemon::read_frame(&mut stream)?;
        let req = Request::decode(&payload)?;
        let resp = handle(store, &req);
        daemon::write_frame(&mut stream, &resp.encode())?;
        Ok(())
    }

    /// Runs a request, turning any error into a `fatal:` stderr + exit 128 — the
    /// same shape the `alt` binary reports for an uncaught error.
    fn handle(store: &mut Store, req: &Request) -> Response {
        match dispatch(store, req) {
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

    fn dispatch(store: &mut Store, req: &Request) -> Res<(u8, Vec<u8>)> {
        // the request argv carries no program name; clap expects one
        let argv = std::iter::once("alt".to_owned()).chain(req.args.iter().cloned());
        let cli = Cli::try_parse_from(argv)?;
        let id = Identity::from_map(&req.env);
        // per-request catch-up: see writes other processes committed since the
        // last request, so a served read is never stale
        store.refresh()?;
        let mut out = Vec::new();
        let code = cli::run_on_store(&cli, store, &req.cwd, id, &mut out)?;
        Ok((code, out))
    }
}
