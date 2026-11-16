//! The daemon of a test repository, ended when its test is over.

/// Ends the daemon of a test repository and waits until it is gone. A
/// daemon still there when its test ends is expected to watch its tree
/// still, unless the second field says not: one that went back to
/// scanning on the way fails the test.
pub(crate) struct Stop(pub(crate) i32, pub(crate) bool);

impl Stop {
    /// For a test that puts a directory where an ignore file is looked
    /// for: its daemon gives up watching and is not held to it.
    pub(crate) fn unheld(mut self) -> Stop {
        self.1 = false;
        self
    }
}

impl Drop for Stop {
    fn drop(&mut self) {
        let fds = std::fs::read_dir(format!("/proc/{}/fd", self.0));
        let links = |fds: std::fs::ReadDir| fds.filter_map(|fd| fd.ok()?.path().read_link().ok());
        let watch = |to: std::path::PathBuf| to.to_string_lossy().contains("inotify");
        let gave_up = fds.is_ok_and(|fds| !links(fds).any(watch));
        let _ = alt_os::sys::kill(self.0, alt_os::sys::SIGTERM);
        let mut waits = 500;
        while waits > 0 && alt_os::sys::kill(self.0, 0).is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(10));
            waits -= 1;
        }
        if waits == 0 {
            let _ = alt_os::sys::kill(self.0, alt_os::sys::SIGKILL);
        }
        if gave_up && self.1 && !std::thread::panicking() {
            panic!(
                "the daemon {} did not watch its tree when the test ended",
                self.0
            );
        }
    }
}
