//! What ends the server: SIGINT (Ctrl-C) and SIGTERM (`systemctl stop`,
//! `docker stop`, k8s preStop) set a flag the main thread looks at.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set when SIGINT or SIGTERM arrives. Read by the main thread to
/// drive `Server::unblock()` and graceful worker drain.
pub(super) static SHUTDOWN: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn shutdown_handler(_sig: alt_os::sys::c_int) {
    // Async-signal-safe: relaxed atomic store is the only thing we do
    // inside the handler. The main thread does the actual work.
    SHUTDOWN.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
pub(super) fn install_signal_handlers() {
    let handler = shutdown_handler as *const () as alt_os::sys::sighandler_t;
    unsafe {
        alt_os::sys::signal(alt_os::sys::SIGINT, handler);
        alt_os::sys::signal(alt_os::sys::SIGTERM, handler);
    }
}

#[cfg(not(unix))]
pub(super) fn install_signal_handlers() {
    // Non-unix targets: no handler. The shutdown loop still runs but
    // `SHUTDOWN` is never set externally, so the process exits when
    // an operator hard-kills it.
}
