//! The `altd-server` a test talks to: started on a port of its own with
//! a directory of its own for what it keeps for a while, and killed with
//! the test.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) struct Server {
    pub(crate) child: Child,
    pub(crate) addr: String,
    pub(crate) stderr: Option<BufReader<std::process::ChildStderr>>,
    // what the server keeps while it takes a push, which a server killed
    // with a request just answered leaves behind
    _temporary: alt_os::temp::TempDir,
}

impl Server {
    /// Kill the server, then drain everything it wrote to stderr since
    /// the bind line. Reading must happen *after* kill since the child's
    /// stderr fd stays open until the process exits, and a blocking
    /// `read_to_string` would otherwise deadlock waiting for EOF.
    pub(crate) fn drain_stderr(&mut self) -> String {
        // tiny_http writes the response back from a worker thread; the
        // access-log JSON line is emitted right after, but the OS may
        // schedule the parent's drain in between the response and the
        // log write. A short settle window lets the worker finish its
        // stderr write before the SIGKILL closes the pipe.
        std::thread::sleep(Duration::from_millis(100));
        let _ = self.child.kill();
        let _ = self.child.wait();
        let mut out = String::new();
        if let Some(mut reader) = self.stderr.take() {
            // Read from the BufReader, not the inner ChildStderr:
            // the bind-line `read_line` may have buffered additional
            // bytes (BufReader fills its 8 KiB buffer on the first
            // syscall), and reading the inner stream directly would
            // skip past whatever access-log JSON-lines landed in the
            // same syscall as the bind line.
            let _ = reader.read_to_string(&mut out);
        }
        out
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `altd-server` on a kernel-chosen port and wait until it logs
/// the bind address — that's the handshake telling the test the server
/// is actually ready to accept the next request. Single-repo mode.
pub(crate) fn spawn_server(repo_dir: &Path) -> Server {
    spawn_server_with_env(&[("ALT_SERVER_REPO", repo_dir.to_str().unwrap())])
}

pub(crate) fn spawn_server_with_env(env: &[(&str, &str)]) -> Server {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_altd-server"));
    cmd.args(["--bind", "127.0.0.1:0"]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    listening(cmd)
}

/// The server `cmd` starts, once it listens: `cmd` runs `altd-server`
/// with `--bind 127.0.0.1:0`, directly or through a program that execs it.
pub(crate) fn listening(mut cmd: Command) -> Server {
    let temporary = alt_os::temp::tempdir().unwrap();
    cmd.env("TMPDIR", temporary.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn altd-server");
    let mut reader = BufReader::new(child.stderr.take().expect("server stderr"));
    let mut said = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let addr = loop {
        let before = said.len();
        if (&mut reader).take(1 << 16).read_line(&mut said).unwrap() == 0 {
            let ended = child.wait();
            panic!("altd-server ended before it listened ({ended:?}), having said: {said}");
        }
        if let Some((_, after)) = said[before..].split_once("listening on ") {
            break after.split_whitespace().next().unwrap().to_owned();
        }
        if Instant::now() > deadline || said.len() > 1 << 20 {
            panic!("altd-server never logged its listening address, having said: {said}");
        }
    };
    Server {
        child,
        addr,
        stderr: Some(reader),
        _temporary: temporary,
    }
}
