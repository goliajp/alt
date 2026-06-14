//! The `alt` client routing hot commands through the per-repo `altd` daemon, so
//! each invocation skips the store open (mmap packs, read `map.alt`, replay the
//! op log). The daemon is a transparent performance cache; local-first means it
//! is never a dependency.
//!
//! Routed commands: the reads (`status`/`branch`/`diff` against the held
//! `Store`, `log` against the held `Repository`) and, since D4, the native
//! writes (`add`/`commit`/`switch`/`merge`/`flow`/`undo`).
//!
//! Fallback for a read is **at-most-once**: a read is idempotent, so any failure
//! falls through to running it directly.
//!
//! A write is **exactly-once** (D5c). It carries a client-chosen idempotency id;
//! the daemon stamps it on the ref transaction and, on a same-id retry, detects
//! the write as already applied and acks instead of re-running it (the index is
//! durable, so this holds even across a daemon restart). So [`serve_write`]:
//! generates one id, sends it, and — if the response is lost after the request
//! went out ([`Outcome::LostAfterSend`]) — retries with the *same* id until it
//! gets a response or a deadline elapses. The only path that falls back to a
//! keyless direct run is a first-attempt [`Outcome::NotSent`] (the request never
//! reached any daemon, so nothing was applied and a direct run cannot double
//! it). Once a request has gone out, the client never falls back to a direct run
//! (a keyless run cannot be deduplicated); it either gets an ack or, after the
//! deadline, surfaces the at-most-once error.

#[cfg(unix)]
pub use imp::{Outcome, disabled, is_idempotent, routes_through_daemon, serve_write, try_serve};

#[cfg(unix)]
mod imp {
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use crate::cli::Command as Cmd;
    use crate::daemon::{self, Request, RequestId, Response};

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

    /// Whether re-running the command is harmless — the reads. A bare `branch`
    /// (no name, no `-d`) lists branches and so is a read; `branch <name>` and
    /// `branch -d` mutate refs and are writes. Writes route through
    /// [`serve_write`] (exactly-once via an idempotency id), reads through
    /// [`try_serve`] (at-most-once direct fallback).
    pub fn is_idempotent(cmd: &Cmd) -> bool {
        matches!(
            cmd,
            Cmd::Status { .. }
                | Cmd::Diff { .. }
                | Cmd::Log(_)
                | Cmd::Branch {
                    name: None,
                    delete: None,
                    ..
                }
        )
    }

    /// `ALT_NO_DAEMON` (any value) forces the direct path — the escape hatch the
    /// local-first contract promises.
    pub fn disabled() -> bool {
        std::env::var_os("ALT_NO_DAEMON").is_some()
    }

    /// How long [`serve_write`] keeps retrying a same-id write whose response
    /// was lost before it gives up and surfaces the at-most-once error.
    const WRITE_RETRY_BUDGET: Duration = Duration::from_secs(10);

    /// Serves a read through the daemon for the repo at `alt_dir`, spawning one
    /// if none is listening. Reads are idempotent, so this is a single attempt
    /// with no id: the caller falls back to a direct run on `NotSent` *or*
    /// `LostAfterSend`.
    pub fn try_serve(alt_dir: &Path, args: &[String]) -> Outcome {
        send_once(alt_dir, args, None)
    }

    /// Serves a non-idempotent write through the daemon with exactly-once
    /// semantics. Generates one idempotency id and sends it; if the request went
    /// out but the response was lost, retries with the *same* id until a response
    /// arrives or [`WRITE_RETRY_BUDGET`] elapses — the daemon dedups a completed
    /// write (durably, so even across a respawn). Returns:
    /// - `Served` once any attempt gets a response (the daemon ran it, or acked a
    ///   prior application);
    /// - `NotSent` only if the *first* attempt never reached a daemon (nothing was
    ///   applied, so the caller may safely run it directly);
    /// - `LostAfterSend` if the budget runs out after the request had gone out —
    ///   the caller surfaces the at-most-once error rather than risk a double run
    ///   (a direct run carries no id and could not be deduplicated).
    pub fn serve_write(alt_dir: &Path, args: &[String]) -> Outcome {
        let id = new_request_id();
        let mut sent = false;
        let deadline = Instant::now() + WRITE_RETRY_BUDGET;
        loop {
            match send_once(alt_dir, args, Some(id)) {
                Outcome::Served(resp) => return Outcome::Served(resp),
                // first attempt never reached a daemon → nothing ran, fall back
                Outcome::NotSent if !sent => return Outcome::NotSent,
                // either the request went out and the response was lost, or a
                // later attempt couldn't reconnect. The write may have applied,
                // so we must not fall back to a keyless direct run — keep
                // retrying the same id (a respawned daemon rebuilds the durable
                // dedup index and will ack a completed write) until the deadline.
                Outcome::LostAfterSend | Outcome::NotSent => {
                    sent = true;
                    if Instant::now() >= deadline {
                        return Outcome::LostAfterSend;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    /// One attempt to serve `args` (the argv tail, no program name) through the
    /// daemon, carrying optional idempotency `id`, spawning a daemon if none is
    /// listening. The boundary between `NotSent` and `LostAfterSend` is whether
    /// the request frame was fully written: a partial/failed write leaves no
    /// decodable frame, so the daemon never executes; once the frame is out, it
    /// may.
    fn send_once(alt_dir: &Path, args: &[String], id: Option<RequestId>) -> Outcome {
        let sock = alt_dir.join("daemon.sock");
        let (Some(mut stream), Ok(req)) = (
            connect_or_spawn(alt_dir, &sock),
            Request::from_env(args.to_vec(), id),
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

    /// A fresh idempotency id, unique per call: process id and start time make
    /// it unique across invocations (so two separate commands never collide),
    /// and a process-local counter covers the rare case of one process issuing
    /// several writes. All retries of one write reuse the value, so the daemon
    /// can match a retry to the original.
    fn new_request_id() -> RequestId {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut id = [0u8; 16];
        id[0..4].copy_from_slice(&pid.to_le_bytes());
        id[4..12].copy_from_slice(&nanos.to_le_bytes());
        id[12..16].copy_from_slice(&seq.to_le_bytes());
        id
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
