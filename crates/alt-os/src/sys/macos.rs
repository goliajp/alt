//! macOS values and layouts; identical on x86_64 and aarch64.

use super::{c_char, c_int, c_long, c_uint, c_ulong};

pub type mode_t = u16;
pub type nfds_t = c_uint;
/// The type of `setrlimit`'s resource argument.
pub type rlimit_resource_t = c_int;

pub const O_NONBLOCK: c_int = 0x4;
pub const O_NOFOLLOW: c_int = 0x100;
pub const O_DIRECTORY: c_int = 0x0010_0000;
pub const O_CLOEXEC: c_int = 0x0100_0000;

pub const AT_FDCWD: c_int = -2;
pub const AT_SYMLINK_NOFOLLOW: c_int = 0x0020;

pub const SIGCONT: c_int = 19;

pub const RLIMIT_NOFILE: rlimit_resource_t = 8;

pub const SOL_SOCKET: c_int = 0xffff;
pub const SO_RCVBUF: c_int = 0x1002;

pub const RENAME_EXCL: c_uint = 0x0000_0004;

/// `statfs::f_flags` bit: the filesystem is stored locally.
pub const MNT_LOCAL: c_int = 0x0000_1000;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct stat {
    pub st_dev: i32,
    pub st_mode: mode_t,
    pub st_nlink: u16,
    pub st_ino: u64,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: i32,
    pub st_atime: c_long,
    pub st_atime_nsec: c_long,
    pub st_mtime: c_long,
    pub st_mtime_nsec: c_long,
    pub st_ctime: c_long,
    pub st_ctime_nsec: c_long,
    pub st_birthtime: c_long,
    pub st_birthtime_nsec: c_long,
    pub st_size: i64,
    pub st_blocks: i64,
    pub st_blksize: i32,
    pub st_flags: u32,
    pub st_gen: u32,
    pub st_lspare: i32,
    pub st_qspare: [i64; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct fsid_t {
    pub val: [i32; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct statfs {
    pub f_bsize: u32,
    pub f_iosize: i32,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_fsid: fsid_t,
    pub f_owner: u32,
    pub f_type: u32,
    pub f_flags: u32,
    pub f_fssubtype: u32,
    pub f_fstypename: [c_char; 16],
    pub f_mntonname: [c_char; 1024],
    pub f_mntfromname: [c_char; 1024],
    pub f_flags_ext: u32,
    pub f_reserved: [u32; 7],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct statvfs {
    pub f_bsize: c_ulong,
    pub f_frsize: c_ulong,
    pub f_blocks: c_uint,
    pub f_bfree: c_uint,
    pub f_bavail: c_uint,
    pub f_files: c_uint,
    pub f_ffree: c_uint,
    pub f_favail: c_uint,
    pub f_fsid: c_ulong,
    pub f_flag: c_ulong,
    pub f_namemax: c_ulong,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct dirent {
    pub d_ino: u64,
    pub d_seekoff: u64,
    pub d_reclen: u16,
    pub d_namlen: u16,
    pub d_type: u8,
    pub d_name: [c_char; 1024],
}

unsafe extern "C" {
    /// The address of the calling thread's `errno`.
    #[link_name = "__error"]
    pub fn errno_location() -> *mut c_int;

    pub fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
}
