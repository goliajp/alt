//! An open directory and the calls that take names relative to it, so that
//! a walk resolves each name once instead of the whole path every time.

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use alt_os::sys;

use crate::cachetree::key_cmp;
use crate::snapshot::FileStat;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir,
    File,
    Link,
    Other,
}

/// The names in a directory, each followed by a NUL byte.
#[derive(Default)]
pub(crate) struct Listing {
    bytes: Vec<u8>,
    items: Vec<(u32, u32, Kind)>,
}

impl Listing {
    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn name(&self, i: usize) -> &[u8] {
        let (at, len, _) = self.items[i];
        &self.bytes[at as usize..(at + len) as usize]
    }

    pub(crate) fn c_name(&self, i: usize) -> &CStr {
        let (at, len, _) = self.items[i];
        CStr::from_bytes_with_nul(&self.bytes[at as usize..(at + len) as usize + 1])
            .expect("names are stored with their terminator")
    }

    pub(crate) fn kind(&self, i: usize) -> Kind {
        self.items[i].2
    }

    pub(crate) fn sort(&mut self) {
        let bytes = &self.bytes;
        self.items.sort_unstable_by(|a, b| {
            let name = |i: &(u32, u32, Kind)| &bytes[i.0 as usize..(i.0 + i.1) as usize];
            key_cmp(name(a), a.2 == Kind::Dir, name(b), b.2 == Kind::Dir)
        });
    }

    /// Where the sorted listing holds `name` as a directory or as something else.
    pub(crate) fn find(&self, name: &[u8], dir: bool) -> Option<usize> {
        let at = self.items.partition_point(|i| {
            let have = &self.bytes[i.0 as usize..(i.0 + i.1) as usize];
            key_cmp(have, i.2 == Kind::Dir, name, dir) == std::cmp::Ordering::Less
        });
        (at < self.len() && self.name(at) == name && (self.kind(at) == Kind::Dir) == dir)
            .then_some(at)
    }
}

pub(crate) struct DirFd {
    fd: sys::c_int,
    stream: *mut sys::DIR,
}

// the stream is only touched through `&mut self`
unsafe impl Send for DirFd {}
unsafe impl Sync for DirFd {}

impl Drop for DirFd {
    fn drop(&mut self) {
        // SAFETY: both are open and owned by this value; closing the
        // stream closes its descriptor
        unsafe {
            match self.stream.is_null() {
                true => sys::close(self.fd),
                false => sys::closedir(self.stream),
            };
        }
    }
}

fn c_path(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in a path"))
}

fn absent(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

impl DirFd {
    pub(crate) fn open(path: &Path) -> io::Result<DirFd> {
        let path = c_path(path.as_os_str().as_bytes())?;
        DirFd::open_from(sys::AT_FDCWD, &path, 0)
    }

    /// The directory `name` in this one; `None` when there is no such
    /// directory. A symlink is not followed.
    pub(crate) fn open_at(&self, name: &CStr) -> io::Result<Option<DirFd>> {
        match DirFd::open_from(self.fd, name, sys::O_NOFOLLOW) {
            Ok(dir) => Ok(Some(dir)),
            // a symlink in the way fails with ELOOP
            Err(e) if absent(&e) || e.raw_os_error() == Some(ELOOP) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn open_from(at: sys::c_int, name: &CStr, flags: sys::c_int) -> io::Result<DirFd> {
        let flags = sys::O_RDONLY | sys::O_DIRECTORY | sys::O_CLOEXEC | flags;
        // SAFETY: `name` is NUL-terminated and outlives the call
        let fd = unsafe { sys::openat(at, name.as_ptr(), flags) };
        match fd < 0 {
            true => Err(io::Error::last_os_error()),
            false => Ok(DirFd {
                fd,
                stream: std::ptr::null_mut(),
            }),
        }
    }

    /// `lstat` of `name` in this directory; `None` when nothing is there.
    #[allow(
        clippy::unnecessary_cast,
        reason = "the field types differ between systems"
    )]
    pub(crate) fn stat(&self, name: &CStr) -> io::Result<Option<FileStat>> {
        let mut st = std::mem::MaybeUninit::<sys::stat>::uninit();
        // SAFETY: `name` is NUL-terminated and `st` has room for one stat
        let rc = unsafe {
            sys::fstatat(
                self.fd,
                name.as_ptr(),
                st.as_mut_ptr(),
                sys::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            let e = io::Error::last_os_error();
            return match absent(&e) {
                true => Ok(None),
                false => Err(e),
            };
        }
        // SAFETY: the call succeeded, so the kernel filled it in
        let st = unsafe { st.assume_init() };
        Ok(Some(FileStat {
            mtime: (st.st_mtime as i64, st.st_mtime_nsec as u32),
            ctime: (st.st_ctime as i64, st.st_ctime_nsec as u32),
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
            size: st.st_size as u64,
            mode: st.st_mode as u32,
        }))
    }

    /// The content of the file `name`, through a symlink if it is one.
    pub(crate) fn read(&self, name: &CStr) -> io::Result<Vec<u8>> {
        use std::io::Read;
        // SAFETY: `name` is NUL-terminated and outlives the call
        let fd = unsafe { sys::openat(self.fd, name.as_ptr(), sys::O_RDONLY | sys::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: just opened, owned by nobody else
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut out = Vec::new();
        file.read_to_end(&mut out)?;
        Ok(out)
    }

    /// Every name in the directory except `.` and `..`, in no order.
    pub(crate) fn names(&mut self) -> io::Result<Listing> {
        let mut listing = Listing::default();
        let mut unknown = Vec::new();
        let mut add = |name: &[u8], d_type: u8| {
            if name == b".\0" || name == b"..\0" {
                return;
            }
            let kind = match d_type {
                DT_DIR => Kind::Dir,
                DT_REG => Kind::File,
                DT_LNK => Kind::Link,
                DT_UNKNOWN => {
                    unknown.push(listing.items.len());
                    Kind::Other
                }
                _ => Kind::Other,
            };
            let at = listing.bytes.len() as u32;
            listing.bytes.extend_from_slice(name);
            listing.items.push((at, name.len() as u32 - 1, kind));
        };
        self.each_entry(&mut add)?;
        // a filesystem that does not say what an entry is
        for i in unknown {
            if let Some(st) = self.stat(listing.c_name(i))? {
                listing.items[i].2 = kind_of(st.mode);
            }
        }
        Ok(listing)
    }

    /// Calls `add` with every entry's NUL-terminated name and type. The
    /// kernel is asked directly: the C library's directory stream costs
    /// five more calls for each directory.
    #[cfg(target_os = "linux")]
    fn each_entry(&mut self, add: &mut dyn FnMut(&[u8], u8)) -> io::Result<()> {
        // d_ino, d_off, d_reclen, d_type, then the name
        const NAME: usize = 19;
        thread_local! {
            static BUFFER: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(vec![0; 1 << 16]);
        }
        BUFFER.with_borrow_mut(|buf| {
            loop {
                // SAFETY: the buffer is writable for its whole length
                let n = unsafe { sys::getdents64(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    return Err(io::Error::last_os_error());
                }
                if n == 0 {
                    return Ok(());
                }
                let mut at = 0;
                while at < n as usize {
                    let len = u16::from_ne_bytes([buf[at + 16], buf[at + 17]]) as usize;
                    let name = &buf[at + NAME..at + len];
                    let end = name
                        .iter()
                        .position(|b| *b == 0)
                        .expect("the kernel ends names");
                    add(&name[..=end], buf[at + 18]);
                    at += len;
                }
            }
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn each_entry(&mut self, add: &mut dyn FnMut(&[u8], u8)) -> io::Result<()> {
        if self.stream.is_null() {
            // SAFETY: the descriptor is an open directory; the stream owns it from here
            self.stream = unsafe { sys::fdopendir(self.fd) };
            if self.stream.is_null() {
                return Err(io::Error::last_os_error());
            }
        }
        loop {
            sys::clear_errno();
            // SAFETY: the stream is open and only used by this thread
            let entry = unsafe { sys::readdir(self.stream) };
            if entry.is_null() {
                let e = io::Error::last_os_error();
                return match e.raw_os_error() {
                    Some(0) | None => Ok(()),
                    _ => Err(e),
                };
            }
            // SAFETY: a non-null entry is valid until the next call and its name is NUL-terminated
            unsafe {
                let name = CStr::from_ptr((&raw const (*entry).d_name).cast());
                add(name.to_bytes_with_nul(), (*entry).d_type);
            }
        }
    }
}

#[allow(
    clippy::unnecessary_cast,
    reason = "the constants' type differs between systems"
)]
pub(crate) fn kind_of(st_mode: u32) -> Kind {
    match st_mode & sys::S_IFMT as u32 {
        m if m == sys::S_IFDIR as u32 => Kind::Dir,
        m if m == sys::S_IFREG as u32 => Kind::File,
        m if m == sys::S_IFLNK as u32 => Kind::Link,
        _ => Kind::Other,
    }
}

const DT_UNKNOWN: u8 = 0;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

#[cfg(target_os = "linux")]
const ELOOP: i32 = 40;
#[cfg(not(target_os = "linux"))]
const ELOOP: i32 = 62;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn a_directory_sorts_as_its_name_with_a_slash() {
        assert_eq!(key_cmp(b"a", true, b"a.txt", false), Greater);
        assert_eq!(key_cmp(b"a", true, b"a0", false), Less);
        assert_eq!(key_cmp(b"a", false, b"a", true), Less);
        assert_eq!(key_cmp(b"a", true, b"a", true), Equal);
        assert_eq!(key_cmp(b"ab", false, b"a", true), Greater);
        assert_eq!(key_cmp(b"a", false, b"ab", true), Less);
    }

    #[test]
    fn listing_names_kinds_and_lookups() {
        let tmp = alt_os::temp::tempdir().unwrap();
        std::fs::write(tmp.path().join("b.txt"), b"x").unwrap();
        std::fs::create_dir(tmp.path().join("b")).unwrap();
        std::os::unix::fs::symlink("b.txt", tmp.path().join("l")).unwrap();
        let mut dir = DirFd::open(tmp.path()).unwrap();
        let mut names = dir.names().unwrap();
        names.sort();
        let seen: Vec<(&[u8], Kind)> = (0..names.len())
            .map(|i| (names.name(i), names.kind(i)))
            .collect();
        assert_eq!(
            seen,
            vec![
                (&b"b.txt"[..], Kind::File),
                (b"b", Kind::Dir),
                (b"l", Kind::Link)
            ]
        );
        assert_eq!(names.find(b"b", true), Some(1));
        assert_eq!(names.find(b"b", false), None);
        assert_eq!(names.find(b"l", false), Some(2));
        let st = dir.stat(c"b.txt").unwrap().unwrap();
        assert_eq!((st.size, kind_of(st.mode)), (1, Kind::File));
        assert!(dir.stat(c"missing").unwrap().is_none());
        assert!(dir.open_at(c"b").unwrap().is_some());
        assert!(dir.open_at(c"l").unwrap().is_none());
        assert!(dir.open_at(c"missing").unwrap().is_none());
        assert_eq!(dir.read(c"l").unwrap(), b"x");
    }
}
