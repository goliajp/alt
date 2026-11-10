use crate::{
    Error, Result, checked_path,
    manifest::{EntryKind, Node, Tree},
};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::Path,
};

/// Directory-relative access. Every parent is opened without following links,
/// so even concurrently replaced parents cannot redirect reads outside root.
pub struct Root(File);

fn cstr(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| Error::Unsupported("NUL in path".into()))
}
fn open_at(parent: &File, name: &str, flags: i32) -> Result<File> {
    let name = cstr(name)?;
    // SAFETY: name is NUL terminated, and a successful descriptor is owned.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
impl Root {
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        Ok(Self(
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)?,
        ))
    }
    fn parent(&self, path: &str) -> Result<(File, String)> {
        checked_path(path.as_bytes())?;
        let mut parts = path.split('/').peekable();
        let mut directory = self.0.try_clone()?;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                return Ok((directory, part.to_owned()));
            }
            directory = open_at(&directory, part, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
        unreachable!("checked nonempty path")
    }
    pub fn copy(&self, path: &str, output: &mut dyn Write) -> Result<Node> {
        let (parent, name) = self.parent(path)?;
        let mut file = open_at(&parent, &name, libc::O_RDONLY | libc::O_NONBLOCK)?;
        let before = file.metadata()?;
        if !before.is_file() {
            return Err(Error::Unsupported(format!("not a regular file: {path}")));
        }
        let mut hash = alt_hash::blake3::Hasher::new();
        let mut len = 0;
        let mut buffer = [0u8; 65536];
        let mut remaining = before.len().saturating_add(1);
        loop {
            if remaining == 0 {
                break;
            }
            let limit = remaining.min(buffer.len() as u64) as usize;
            let n = file.read(&mut buffer[..limit])?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
            output.write_all(&buffer[..n])?;
            len += n as u64;
            remaining -= n as u64;
        }
        let after = file.metadata()?;
        let current = open_at(&parent, &name, libc::O_RDONLY | libc::O_NONBLOCK)?.metadata()?;
        let signature = |m: &std::fs::Metadata| {
            (
                m.dev(),
                m.ino(),
                m.mode(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        if signature(&before) != signature(&after)
            || signature(&before) != signature(&current)
            || len != before.len()
        {
            return Err(Error::Changed(path.into()));
        }
        Ok(Node {
            kind: EntryKind::File,
            executable: before.mode() & 0o111 != 0,
            len,
            content: hash.finalize().to_hex().to_string(),
            link_target: None,
        })
    }
}

impl Tree for Root {
    fn inspect(&self, path: &str) -> Result<Node> {
        let (parent, name) = self.parent(path)?;
        let name_c = cstr(&name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name_c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let stat = unsafe { stat.assume_init() };
        match stat.st_mode & libc::S_IFMT {
            libc::S_IFREG => self.copy(path, &mut std::io::sink()),
            libc::S_IFDIR => Ok(Node::directory()),
            libc::S_IFLNK => {
                let mut bytes = vec![0u8; 65536];
                let len = unsafe {
                    libc::readlinkat(
                        parent.as_raw_fd(),
                        name_c.as_ptr(),
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                    )
                };
                if len < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                if len as usize == bytes.len() {
                    return Err(Error::Unsupported("link target too long".into()));
                }
                bytes.truncate(len as usize);
                crate::manifest::checked_link(path, &bytes)?;
                Ok(Node {
                    kind: EntryKind::Symlink,
                    executable: false,
                    len: bytes.len() as u64,
                    content: alt_hash::blake3::hash(&bytes).to_hex().to_string(),
                    link_target: Some(crate::hex(&bytes)),
                })
            }
            _ => Err(Error::Unsupported(format!("special input file: {path}"))),
        }
    }
    fn children(&self, path: &str) -> Result<Vec<String>> {
        let (parent, name) = self.parent(path)?;
        let directory = open_at(&parent, &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
        use std::os::fd::IntoRawFd;
        let fd = directory.into_raw_fd();
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            unsafe { libc::close(fd) };
            return Err(std::io::Error::last_os_error().into());
        }
        struct Directory(*mut libc::DIR);
        impl Drop for Directory {
            fn drop(&mut self) {
                unsafe { libc::closedir(self.0) };
            }
        }
        let directory = Directory(stream);
        let mut children = Vec::new();
        loop {
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0;
            }
            #[cfg(target_os = "macos")]
            unsafe {
                *libc::__error() = 0;
            }
            let entry = unsafe { libc::readdir(directory.0) };
            if entry.is_null() {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(error.into());
                }
                break;
            }
            let bytes = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            children.push(format!("{path}/{}", checked_path(bytes)?));
        }
        children.sort();
        Ok(children)
    }
}
