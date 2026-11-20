//! Where watching a directory does not tell of every change in it: on a
//! filesystem that other machines write to, and across the mount point of
//! another filesystem inside the tree.

use std::fs::File;
use std::io::{self, Read, Seek};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use alt_os::sys;

/// The filesystems whose files change without an event on this machine:
/// served over a network, shared between the nodes of a cluster, passed
/// in from a host, or kept by a process of its own.
const ELSEWHERE: [i64; 23] = [
    0x6969,      // nfs
    0x517b,      // smb
    0xff53_4d42, // cifs
    0xfe53_4d42, // smb2
    0x6573_5546, // fuse, virtiofs
    0x0102_1997, // 9p
    0x00c3_6400, // ceph
    0x5346_414f, // afs
    0x6b41_4653, // kafs
    0x7375_7245, // coda
    0x564c,      // ncp
    0x0116_1970, // gfs2
    0x7461_636f, // ocfs2
    0x0bd0_0bd0, // lustre
    0x4750_4653, // gpfs
    0xaad7_aaea, // panfs
    0x0131_11a8, // ibrix
    0x6163_6673, // acfs
    0xa501_fcf5, // vxfs
    0x786f_4256, // vboxsf
    0xbacb_acbc, // vmhgfs
    0x7c7c_6673, // prl_fs
    0xbeef_dead, // snfs
];

pub(crate) fn written_elsewhere(kind: i64) -> bool {
    // the kernel hands the 32 bits out sign-extended or not
    ELSEWHERE.contains(&(kind & 0xffff_ffff))
}

/// Whether the directory at `root` is on such a filesystem.
pub(crate) fn elsewhere(root: &Path) -> io::Result<bool> {
    let dir = File::open(root)?;
    let mut stat = std::mem::MaybeUninit::<sys::statfs>::uninit();
    // SAFETY: the descriptor is open and `stat` has room for the structure
    if unsafe { sys::fstatfs(dir.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatfs succeeded and filled the structure
    Ok(written_elsewhere(unsafe { stat.assume_init() }.f_type))
}

const POLLPRI: i16 = 0x2;
const POLLERR: i16 = 0x8;

/// The table of what is mounted where, as this process sees it.
pub(crate) struct Mounts(File);

impl Mounts {
    pub(crate) fn open() -> io::Result<Mounts> {
        Ok(Mounts(File::open("/proc/self/mountinfo")?))
    }

    /// Whether anything was mounted or unmounted since this was last
    /// asked, or since the table was opened.
    pub(crate) fn changed(&mut self) -> io::Result<bool> {
        let mut table = sys::pollfd {
            fd: self.0.as_raw_fd(),
            events: POLLPRI,
            revents: 0,
        };
        // SAFETY: one structure is passed and one is announced
        match unsafe { sys::poll(&mut table, 1, 0) } {
            n if n < 0 => Err(io::Error::last_os_error()),
            _ => Ok(table.revents & (POLLPRI | POLLERR) != 0),
        }
    }

    /// Whether a filesystem is mounted on a directory beneath `root`.
    pub(crate) fn beneath(&mut self, root: &Path) -> io::Result<bool> {
        let root = std::fs::canonicalize(root)?;
        let mut table = Vec::new();
        self.0.rewind()?;
        self.0.read_to_end(&mut table)?;
        Ok(mounted_beneath(&table, root.as_os_str().as_bytes()))
    }
}

/// Whether `table`, in the form of `/proc/self/mountinfo`, has a mount
/// point beneath the directory `root`.
fn mounted_beneath(table: &[u8], root: &[u8]) -> bool {
    let root = root.strip_suffix(b"/").unwrap_or(root);
    table.split(|b| *b == b'\n').any(|line| {
        // the fifth field, with space, tab, newline and backslash in octal
        let Some(point) = line.split(|b| *b == b' ').nth(4) else {
            return false;
        };
        let point = unescaped(point);
        point.len() > root.len() && point[root.len()] == b'/' && point.starts_with(root)
    })
}

fn unescaped(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut at = 0;
    while at < field.len() {
        let octal = field.get(at + 1..at + 4).filter(|_| field[at] == b'\\');
        let code = octal
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        match code {
            Some(byte) => {
                out.push(byte);
                at += 4;
            }
            None => {
                out.push(field[at]);
                at += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystems_written_from_elsewhere_are_told_from_local_ones() {
        // ext4, xfs, btrfs, tmpfs, overlayfs
        for local in [0xef53, 0x5846_5342, 0x9123_683e, 0x0102_1994, 0x794c_7630] {
            assert!(!written_elsewhere(local), "{local:#x}");
        }
        assert!(written_elsewhere(0x6969));
        assert!(written_elsewhere(0x6573_5546));
        assert!(written_elsewhere(0x0102_1997));
        assert!(written_elsewhere(0x7c7c_6673));
        assert!(written_elsewhere(0xbeef_dead_u32 as i32 as i64));
        // cifs with the sign carried into the upper half
        assert!(written_elsewhere(0xff53_4d42_u32 as i32 as i64));
        assert!(written_elsewhere(0xff53_4d42));
    }

    #[test]
    fn a_mount_point_beneath_the_root_is_found_in_the_table() {
        let table = b"\
22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
30 22 0:26 / /home/u/repo/data rw shared:5 - tmpfs tmpfs rw\n\
31 22 0:27 / /home/u/with\\040space/sub rw - tmpfs tmpfs rw\n\
32 22 0:28 / /home/u/repository rw - tmpfs tmpfs rw\n";
        assert!(mounted_beneath(table, b"/home/u/repo"));
        assert!(mounted_beneath(table, b"/home/u/repo/"));
        assert!(mounted_beneath(table, b"/home/u/with space"));
        // the root itself and a name that only begins like it are not beneath
        assert!(!mounted_beneath(table, b"/home/u/repo/data"));
        assert!(!mounted_beneath(table, b"/home/u/repositor"));
        assert!(!mounted_beneath(table, b"/home/u/other"));
        assert!(mounted_beneath(table, b"/home"));
        assert_eq!(unescaped(b"a\\134b\\011c\\"), b"a\\b\tc\\");
    }

    #[test]
    fn the_directory_tests_run_in_is_watchable() {
        let dir = alt_os::temp::tempdir().unwrap();
        assert!(!elsewhere(dir.path()).unwrap());
        let mut mounts = Mounts::open().unwrap();
        assert!(!mounts.beneath(dir.path()).unwrap());
        assert!(mounts.beneath(Path::new("/")).unwrap());
    }
}
