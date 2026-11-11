//! Linux (glibc) values and layouts for x86_64 and aarch64.

use super::{c_char, c_int, c_uint, c_ulong, c_void};

pub type mode_t = u32;
pub type nfds_t = c_ulong;
/// The type of `setrlimit`'s resource argument.
pub type rlimit_resource_t = c_uint;

pub const O_NONBLOCK: c_int = 0o4000;
pub const O_CLOEXEC: c_int = 0o2000000;
#[cfg(target_arch = "x86_64")]
pub const O_DIRECTORY: c_int = 0o200000;
#[cfg(target_arch = "x86_64")]
pub const O_NOFOLLOW: c_int = 0o400000;
#[cfg(target_arch = "aarch64")]
pub const O_DIRECTORY: c_int = 0o40000;
#[cfg(target_arch = "aarch64")]
pub const O_NOFOLLOW: c_int = 0o100000;

pub const AT_FDCWD: c_int = -100;
pub const AT_SYMLINK_NOFOLLOW: c_int = 0x100;

pub const SIGCONT: c_int = 18;

pub const RLIMIT_NOFILE: rlimit_resource_t = 7;

pub const SOL_SOCKET: c_int = 1;
pub const SO_RCVBUF: c_int = 8;

pub const RENAME_NOREPLACE: c_uint = 1;

pub const MS_NOSUID: c_ulong = 0x02;
pub const MS_NODEV: c_ulong = 0x04;
pub const MS_NOEXEC: c_ulong = 0x08;
pub const MNT_DETACH: c_int = 0x2;

#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: mode_t,
    pub st_uid: u32,
    pub st_gid: u32,
    __pad0: c_int,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: i64,
    pub st_mtime: i64,
    pub st_mtime_nsec: i64,
    pub st_ctime: i64,
    pub st_ctime_nsec: i64,
    __unused: [i64; 3],
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: mode_t,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    __pad1: u64,
    pub st_size: i64,
    pub st_blksize: i32,
    __pad2: c_int,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: i64,
    pub st_mtime: i64,
    pub st_mtime_nsec: i64,
    pub st_ctime: i64,
    pub st_ctime_nsec: i64,
    __unused: [c_int; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct fsid_t {
    __val: [c_int; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct statfs {
    pub f_type: i64,
    pub f_bsize: i64,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_fsid: fsid_t,
    pub f_namelen: i64,
    pub f_frsize: i64,
    f_spare: [i64; 5],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct statvfs {
    pub f_bsize: c_ulong,
    pub f_frsize: c_ulong,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_favail: u64,
    pub f_fsid: c_ulong,
    pub f_flag: c_ulong,
    pub f_namemax: c_ulong,
    __f_spare: [c_int; 6],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct dirent {
    pub d_ino: u64,
    pub d_off: i64,
    pub d_reclen: u16,
    pub d_type: u8,
    pub d_name: [c_char; 256],
}

unsafe extern "C" {
    /// The address of the calling thread's `errno`.
    #[link_name = "__errno_location"]
    pub fn errno_location() -> *mut c_int;

    pub fn renameat2(
        olddirfd: c_int,
        oldpath: *const c_char,
        newdirfd: c_int,
        newpath: *const c_char,
        flags: c_uint,
    ) -> c_int;
    pub fn mount(
        src: *const c_char,
        target: *const c_char,
        fstype: *const c_char,
        flags: c_ulong,
        data: *const c_void,
    ) -> c_int;
    pub fn umount2(target: *const c_char, flags: c_int) -> c_int;
}
