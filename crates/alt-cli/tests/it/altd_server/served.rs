//! The `altd-server` a test talks to: started on a port of its own with
//! a directory of its own for what it keeps for a while, and killed with
//! the test.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) struct Server {
    pub(super) child: Child,
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
    #[allow(dead_code)]
    pub(super) fn drain_stderr(&mut self) -> String {
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
            use std::io::Read;
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

pub(super) fn spawn_server_with_env(env: &[(&str, &str)]) -> Server {
    let bin = env!("CARGO_BIN_EXE_altd-server");
    let temporary = alt_os::temp::tempdir().unwrap();
    let mut cmd = Command::new(bin);
    cmd.args(["--bind", "127.0.0.1:0"])
        .env("TMPDIR", temporary.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn altd-server");
    let stderr = child.stderr.take().expect("server stderr");
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let addr = loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if let Some(rest) = line.find("listening on ") {
            let after = &line[rest + "listening on ".len()..];
            let addr = after.split_whitespace().next().unwrap().to_owned();
            break addr;
        }
        if Instant::now() > deadline {
            panic!("altd-server never logged its listening address");
        }
    };
    Server {
        child,
        addr,
        stderr: Some(reader),
        _temporary: temporary,
    }
}
