//! The part of the C system interface the workspace calls.
//!
//! Names follow the C headers. Values and layouts that differ between Linux
//! and macOS, or between x86_64 and aarch64, live in the per-system modules
//! and are re-exported here.

#![expect(
    non_camel_case_types,
    reason = "types keep the names they have in the C headers"
)]

pub use core::ffi::{c_char, c_int, c_long, c_short, c_uint, c_ulong, c_void};
use std::{
    io,
    os::fd::{AsFd, AsRawFd},
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;

pub type pid_t = i32;
pub type uid_t = u32;
pub type socklen_t = u32;
/// A signal handler as `signal` takes it: the address of an
/// `extern "C" fn(c_int)`.
pub type sighandler_t = usize;

pub const O_RDONLY: c_int = 0;

pub const LOCK_SH: c_int = 1;
pub const LOCK_EX: c_int = 2;
pub const LOCK_NB: c_int = 4;
pub const LOCK_UN: c_int = 8;

pub const SIGINT: c_int = 2;
pub const SIGKILL: c_int = 9;
pub const SIGTERM: c_int = 15;

pub const POLLIN: c_short = 0x1;

pub const S_IFMT: mode_t = 0o17_0000;
pub const S_IFDIR: mode_t = 0o4_0000;
pub const S_IFREG: mode_t = 0o10_0000;
pub const S_IFLNK: mode_t = 0o12_0000;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct rlimit {
    pub rlim_cur: u64,
    pub rlim_max: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct pollfd {
    pub fd: c_int,
    pub events: c_short,
    pub revents: c_short,
}

/// An open directory stream; only ever handled through a pointer.
pub enum DIR {}

mod raw {
    use super::{c_int, pid_t};

    unsafe extern "C" {
        pub fn flock(fd: c_int, operation: c_int) -> c_int;
        pub fn kill(pid: pid_t, signal: c_int) -> c_int;
    }
}

/// Applies or removes an advisory lock on the open file description behind
/// `file`. `operation` is `LOCK_SH`, `LOCK_EX` or `LOCK_UN`, optionally
/// combined with `LOCK_NB`; with `LOCK_NB` a held lock is reported as
/// [`io::ErrorKind::WouldBlock`].
pub fn flock(file: &impl AsFd, operation: c_int) -> io::Result<()> {
    // SAFETY: the descriptor is open for the duration of the borrow, and
    // flock reads no memory
    if unsafe { raw::flock(file.as_fd().as_raw_fd(), operation) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sends `signal` to a process (`pid > 0`) or a process group (`pid < 0`).
/// Signal `0` sends nothing and only reports whether the target exists.
pub fn kill(pid: pid_t, signal: c_int) -> io::Result<()> {
    // SAFETY: kill takes no pointers; its effect on other processes is
    // outside this process's memory
    if unsafe { raw::kill(pid, signal) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets the calling thread's `errno` to zero, for the calls that report an
/// error only through it.
pub fn clear_errno() {
    // SAFETY: the C library returns a valid pointer to this thread's errno
    unsafe { *errno_location() = 0 };
}

unsafe extern "C" {
    pub safe fn geteuid() -> uid_t;

    #[cfg_attr(
        all(target_os = "macos", target_arch = "x86_64"),
        link_name = "close$NOCANCEL"
    )]
    pub fn close(fd: c_int) -> c_int;
    pub fn openat(dirfd: c_int, pathname: *const c_char, flags: c_int, ...) -> c_int;
    #[cfg_attr(
        all(target_os = "macos", target_arch = "x86_64"),
        link_name = "fstatat$INODE64"
    )]
    pub fn fstatat(dirfd: c_int, pathname: *const c_char, buf: *mut stat, flags: c_int) -> c_int;
    pub fn readlinkat(
        dirfd: c_int,
        pathname: *const c_char,
        buf: *mut c_char,
        bufsiz: usize,
    ) -> isize;
    pub fn mkfifo(path: *const c_char, mode: mode_t) -> c_int;

    #[cfg_attr(
        all(target_os = "macos", target_arch = "x86_64"),
        link_name = "fdopendir$INODE64"
    )]
    pub fn fdopendir(fd: c_int) -> *mut DIR;
    #[cfg_attr(
        all(target_os = "macos", target_arch = "x86_64"),
        link_name = "readdir$INODE64"
    )]
    pub fn readdir(dirp: *mut DIR) -> *mut dirent;
    pub fn closedir(dirp: *mut DIR) -> c_int;

    #[cfg_attr(
        all(target_os = "macos", target_arch = "x86_64"),
        link_name = "fstatfs$INODE64"
    )]
    pub fn fstatfs(fd: c_int, buf: *mut statfs) -> c_int;
    pub fn fstatvfs(fd: c_int, buf: *mut statvfs) -> c_int;

    pub fn setrlimit(resource: rlimit_resource_t, rlim: *const rlimit) -> c_int;
    pub fn signal(signum: c_int, handler: sighandler_t) -> sighandler_t;
    pub fn poll(fds: *mut pollfd, nfds: nfds_t, timeout: c_int) -> c_int;
    pub fn setsockopt(
        socket: c_int,
        level: c_int,
        name: c_int,
        value: *const c_void,
        option_len: socklen_t,
    ) -> c_int;
    pub fn gethostname(name: *mut c_char, len: usize) -> c_int;
}
