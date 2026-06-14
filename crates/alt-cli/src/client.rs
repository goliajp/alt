//! D3: the `alt` client routing hot read commands through the per-repo `altd`
//! daemon, so each invocation skips the ~21ms store open (mmap packs, read
//! `map.alt`, replay the op log). The daemon is a transparent performance cache:
//! local-first means any failure on the way — daemon disabled, no repo, can't
//! reach or spawn the daemon, or a wire error before a complete response — falls
//! through to running the command directly. The daemon is never a dependency.
//!
//! Only *read* commands route here, and only the ones the daemon actually
//! amortizes: the native-store reads (`status`/`branch`/`diff`). Write commands
//! are D4 (they need the in-process group-commit path). Git-layer reads (`log`)
//! reopen their own `Repository` per request inside the daemon, so routing them
//! would add a socket round-trip without amortizing anything — they stay direct.

#[cfg(unix)]
pub use imp::{disabled, routes_through_daemon, try_serve};

#[cfg(unix)]
mod imp {
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use crate::cli::Command as Cmd;
    use crate::daemon::{self, Request, Response};

    /// The read commands whose per-request cost the daemon amortizes by holding
    /// the native store open. See the module docs for why writes and `log` are
    /// excluded.
    pub fn routes_through_daemon(cmd: &Cmd) -> bool {
        matches!(
            cmd,
            Cmd::Status { .. } | Cmd::Branch { .. } | Cmd::Diff { .. }
        )
    }

    /// `ALT_NO_DAEMON` (any value) forces the direct path — the escape hatch the
    /// local-first contract promises.
    pub fn disabled() -> bool {
        std::env::var_os("ALT_NO_DAEMON").is_some()
    }

    /// Serves `args` (the argv tail, no program name) through the daemon for the
    /// repo at `alt_dir`, spawning one if none is listening. Returns the
    /// daemon's response, or `None` meaning "fall back and run directly". All
    /// routed commands are reads, so falling back after a partial wire exchange
    /// re-runs idempotent work — never a double write.
    pub fn try_serve(alt_dir: &Path, args: &[String]) -> Option<Response> {
        let sock = alt_dir.join("daemon.sock");
        let mut stream = connect_or_spawn(alt_dir, &sock)?;
        let req = Request::from_env(args.to_vec()).ok()?;
        daemon::write_frame(&mut stream, &req.encode()).ok()?;
        let payload = daemon::read_frame(&mut stream).ok()?;
        Response::decode(&payload).ok()
    }

    /// Connects to `sock`; if nothing is listening, spawns `altd` and polls for
    /// it to come up (the daemon binds before it accepts). `None` if the daemon
    /// can't be reached — the caller then runs directly.
    fn connect_or_spawn(alt_dir: &Path, sock: &Path) -> Option<UnixStream> {
        if let Ok(s) = UnixStream::connect(sock) {
            return Some(s);
        }
        spawn_daemon(alt_dir)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match UnixStream::connect(sock) {
                Ok(s) => return Some(s),
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return None,
            }
        }
    }

    /// Spawns `altd <alt-dir>` detached as a background daemon. `altd` sits next
    /// to the running `alt` binary. `None` if it can't be located or spawned.
    fn spawn_daemon(alt_dir: &Path) -> Option<()> {
        let altd = std::env::current_exe().ok()?.with_file_name("altd");
        Command::new(altd)
            .arg(alt_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
            .map(|_child| ())
    }
}
