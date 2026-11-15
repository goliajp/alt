//! The three standard descriptors of a program entered through the C `main`.

use core::ffi::{c_char, c_int};

const F_GETFD: c_int = 1;
const O_RDWR: c_int = 2;
const EBADF: c_int = 9;

unsafe extern "C" {
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn abort() -> !;
}

/// Opens `/dev/null` on each of descriptors 0, 1 and 2 that is closed, as
/// the standard library's own start-up does. A program started with one of
/// them closed would otherwise hand that number to the first file it
/// opens, and everything it (or a child it starts) later prints would be
/// written into that file. Call first thing, before anything is opened.
pub fn fill_closed() {
    for fd in closed() {
        // SAFETY: the path is a NUL-terminated literal
        let opened = unsafe { open(c"/dev/null".as_ptr(), O_RDWR, 0) };
        // descriptors are handed out lowest first and are visited in order
        if opened != fd {
            // SAFETY: abort takes no arguments and does not return
            unsafe { abort() };
        }
    }
}

/// Which of the three are closed, in ascending order.
fn closed() -> impl Iterator<Item = c_int> {
    let closed = polled().unwrap_or_else(|| {
        [0, 1, 2].map(|fd| {
            // SAFETY: F_GETFD reads the descriptor's flags and nothing else
            unsafe { fcntl(fd, F_GETFD) == -1 && *crate::sys::errno_location() == EBADF }
        })
    });
    (0..3).filter(move |fd| closed[*fd as usize])
}

/// One `poll` answers for all three descriptors.
#[cfg(target_os = "linux")]
fn polled() -> Option<[bool; 3]> {
    const POLLNVAL: core::ffi::c_short = 0x20;
    let mut fds = [0, 1, 2].map(|fd| crate::sys::pollfd {
        fd,
        events: 0,
        revents: 0,
    });
    // SAFETY: `fds` is a valid array of three pollfd structures
    let ready = unsafe { crate::sys::poll(fds.as_mut_ptr(), 3, 0) };
    (ready >= 0).then(|| fds.map(|fd| fd.revents & POLLNVAL != 0))
}

/// `poll` on macOS rejects some device descriptors outright.
#[cfg(not(target_os = "linux"))]
fn polled() -> Option<[bool; 3]> {
    None
}
