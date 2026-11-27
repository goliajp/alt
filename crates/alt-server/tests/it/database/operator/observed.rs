//! The server processes of these tests, killed when dropped.
use super::*;
use std::time::Instant;

pub(super) struct ServerProcess(pub(super) std::process::Child);
impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A server process whose log is kept in a file.
pub(super) struct ObservedServer {
    process: ServerProcess,
    pub(super) url: String,
    pub(super) log: PathBuf,
}
impl ObservedServer {
    pub(super) fn start(db: &OperatorDb) -> Self {
        let log = db.root.path().join("http-observations.jsonl");
        let stdout = db.root.path().join("server-output.jsonl");
        let process = ServerProcess(
            db.command()
                .arg("serve")
                .env("RUST_LOG", "trace")
                .stdout(std::fs::File::create(&stdout).unwrap())
                .stderr(std::fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(line) = std::fs::read_to_string(&stdout).unwrap().lines().next()
                && let Ok(value) = alt_json::from_str::<alt_json::Value>(line)
                && let Some(address) = value["address"].as_str()
            {
                return Self {
                    process,
                    url: format!("http://{address}"),
                    log,
                };
            }
            assert!(Instant::now() < deadline, "operator did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub(super) fn stop(&mut self) {
        // this unreaped child is owned by this fixture; its PID cannot have
        // been reused while the Child remains unreaped
        alt_os::sys::kill(
            self.process.0.id() as alt_os::sys::pid_t,
            alt_os::sys::SIGTERM,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.process.0.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "server did not stop and drain observations"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
