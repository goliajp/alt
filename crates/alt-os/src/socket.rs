//! What a connected socket still holds of what was written to it.

use crate::sys;
use std::io;
use std::os::fd::AsRawFd;

/// The bytes written to `socket` that its peer has not acknowledged yet:
/// those not sent and those sent and still on their way. What was written
/// less this is what the peer has taken.
pub fn unacknowledged(socket: &impl AsRawFd) -> io::Result<u64> {
    let mut held: sys::c_int = 0;
    #[cfg(target_os = "linux")]
    // SAFETY: the request writes one int where it is pointed to
    let result = unsafe { sys::ioctl(socket.as_raw_fd(), sys::SIOCOUTQ, &raw mut held) };
    #[cfg(target_os = "macos")]
    let result = {
        let mut length = size_of_val(&held) as sys::socklen_t;
        let value = (&raw mut held).cast();
        // SAFETY: the value pointer and its length describe `held`
        unsafe {
            sys::getsockopt(
                socket.as_raw_fd(),
                sys::SOL_SOCKET,
                sys::SO_NWRITE,
                value,
                &raw mut length,
            )
        }
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(held.max(0) as u64)
}
