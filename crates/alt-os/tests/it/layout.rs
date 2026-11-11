//! Sizes and field offsets of the C structures, per target. The numbers are
//! those of the system headers.

use alt_os::sys;
use std::mem::{offset_of, size_of};

#[test]
fn shared_structures() {
    assert_eq!(size_of::<sys::rlimit>(), 16);
    assert_eq!(offset_of!(sys::rlimit, rlim_max), 8);
    assert_eq!(size_of::<sys::pollfd>(), 8);
    assert_eq!(offset_of!(sys::pollfd, events), 4);
    assert_eq!(offset_of!(sys::pollfd, revents), 6);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_structures() {
    assert_eq!(size_of::<sys::stat>(), 144);
    assert_eq!(offset_of!(sys::stat, st_mode), 4);
    assert_eq!(offset_of!(sys::stat, st_ino), 8);
    assert_eq!(offset_of!(sys::stat, st_size), 96);
    assert_eq!(size_of::<sys::statfs>(), 2168);
    assert_eq!(offset_of!(sys::statfs, f_flags), 64);
    assert_eq!(offset_of!(sys::statfs, f_fstypename), 72);
    assert_eq!(size_of::<sys::statvfs>(), 64);
    assert_eq!(offset_of!(sys::statvfs, f_blocks), 16);
    assert_eq!(size_of::<sys::dirent>(), 1048);
    assert_eq!(offset_of!(sys::dirent, d_name), 21);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_structures() {
    #[cfg(target_arch = "x86_64")]
    {
        assert_eq!(size_of::<sys::stat>(), 144);
        assert_eq!(offset_of!(sys::stat, st_mode), 24);
        assert_eq!(offset_of!(sys::stat, st_size), 48);
    }
    #[cfg(target_arch = "aarch64")]
    {
        assert_eq!(size_of::<sys::stat>(), 128);
        assert_eq!(offset_of!(sys::stat, st_mode), 16);
        assert_eq!(offset_of!(sys::stat, st_size), 48);
    }
    assert_eq!(size_of::<sys::statfs>(), 120);
    assert_eq!(offset_of!(sys::statfs, f_type), 0);
    assert_eq!(offset_of!(sys::statfs, f_namelen), 64);
    assert_eq!(size_of::<sys::statvfs>(), 112);
    assert_eq!(offset_of!(sys::statvfs, f_blocks), 16);
    assert_eq!(size_of::<sys::dirent>(), 280);
    assert_eq!(offset_of!(sys::dirent, d_name), 19);
}
