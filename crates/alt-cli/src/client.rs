//! The `alt` client routing hot commands through the per-repo `altd` daemon, so
//! each invocation skips the store open (mmap packs, read `map.alt`, replay the
//! op log). The daemon is a transparent performance cache; local-first means it
//! is never a dependency.
//!
//! Routed commands: the reads (`status`/`branch`/`diff` against the held
//! `Store`, `log` against the held `Repository`) and, since D4, the native
//! writes (`add`/`commit`/`switch`/`merge`/`flow`/`undo`).
//!
//! Fallback is **at-most-once** (D4). A read is idempotent, so any failure falls
//! through to running it directly. A write must not run twice: we fall back only
//! when the request never reached the daemon ([`Outcome::NotSent`] — connect,
//! spawn, or send failed, so the daemon did nothing); if the request was sent
//! but the response was lost ([`Outcome::LostAfterSend`] — the daemon may have
//! already committed), the caller surfaces an error rather than risk a double
//! write. Seamless exactly-once retry (an idempotency token in the protocol) is
//! deferred to D5, where it belongs alongside concurrent request handling.

#[cfg(unix)]
pub use imp::{Outcome, disabled, is_idempotent, routes_through_daemon, try_serve};

#[cfg(unix)]
mod imp {
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use crate::cli::Command as Cmd;
    use crate::daemon::{self, Request, Response};

    /// What came of trying to serve a request through the daemon. The three-way
    /// split exists for the at-most-once write contract: `NotSent` is always
    /// safe to fall back from, `LostAfterSend` is not (for a write).
    pub enum Outcome {
        /// A complete response came back; use it.
        Served(Response),
        /// The request never reached the daemon (couldn't connect, spawn, or
        /// finish sending). The daemon did nothing — safe to run directly.
        NotSent,
        /// The request was sent but the response was lost (the daemon died
        /// mid-flight). The command may have taken effect.
        LostAfterSend,
    }

    /// Whether the daemon serves this command at all: the reads it amortizes via
    /// the held `Store`/`Repository`, plus the native writes (D4).
    pub fn routes_through_daemon(cmd: &Cmd) -> bool {
        matches!(
            cmd,
            Cmd::Status { .. }
                | Cmd::Branch { .. }
                | Cmd::Diff { .. }
                | Cmd::Log(_)
                | Cmd::Add { .. }
                | Cmd::Commit { .. }
                | Cmd::Switch { .. }
                | Cmd::Merge { .. }
                | Cmd::Flow { .. }
                | Cmd::Undo { .. }
        )
    }

    /// Whether re-running the command is harmless. Only the pure reads qualify;
    /// every write is treated as non-idempotent (conservative — the worst case
    /// is asking the user to verify a command that was in fact safe to retry,
    /// never a silent double write).
    pub fn is_idempotent(cmd: &Cmd) -> bool {
        matches!(
            cmd,
            Cmd::Status { .. } | Cmd::Branch { .. } | Cmd::Diff { .. } | Cmd::Log(_)
        )
    }

    /// `ALT_NO_DAEMON` (any value) forces the direct path — the escape hatch the
    /// local-first contract promises.
    pub fn disabled() -> bool {
        std::env::var_os("ALT_NO_DAEMON").is_some()
    }

    /// Serves `args` (the argv tail, no program name) through the daemon for the
    /// repo at `alt_dir`, spawning one if none is listening. The boundary
    /// between `NotSent` and `LostAfterSend` is whether the request frame was
    /// fully written: a partial/failed write leaves no decodable frame, so the
    /// daemon never executes; once the frame is out, it may.
    pub fn try_serve(alt_dir: &Path, args: &[String]) -> Outcome {
        let sock = alt_dir.join("daemon.sock");
        let (Some(mut stream), Ok(req)) = (
            connect_or_spawn(alt_dir, &sock),
            Request::from_env(args.to_vec()),
        ) else {
            return Outcome::NotSent;
        };
        if daemon::write_frame(&mut stream, &req.encode()).is_err() {
            return Outcome::NotSent;
        }
        // the request is out — from here a failure may have left the command
        // executed, so it is no longer safe to fall back for a write
        match daemon::read_frame(&mut stream) {
            Ok(payload) => match Response::decode(&payload) {
                Ok(resp) => Outcome::Served(resp),
                Err(_) => Outcome::LostAfterSend,
            },
            Err(_) => Outcome::LostAfterSend,
        }
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
