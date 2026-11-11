use super::{DEFAULT_PREFIX, create_unique};
use std::{
    ffi::{CString, OsStr},
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    ops::Deref,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

fn create_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Creates a file inside [`std::env::temp_dir`] that has no name: its
/// directory entry is removed before this returns, and the storage is freed
/// when the file is closed.
pub fn tempfile() -> io::Result<File> {
    tempfile_in(std::env::temp_dir())
}

/// Creates a file without a name inside `directory`.
pub fn tempfile_in(directory: impl AsRef<Path>) -> io::Result<File> {
    create_unique(directory.as_ref(), OsStr::new(DEFAULT_PREFIX), |path| {
        let file = create_file(&path)?;
        let _ = std::fs::remove_file(&path);
        Ok(file)
    })
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

/// Renames `from` to `to` and fails with `AlreadyExists` if `to` exists.
fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    use crate::sys;
    let (from_c, to_c) = (c_path(from)?, c_path(to)?);
    // SAFETY: both paths are NUL-terminated strings that outlive the call
    #[cfg(target_os = "linux")]
    let result = unsafe {
        sys::renameat2(
            sys::AT_FDCWD,
            from_c.as_ptr(),
            sys::AT_FDCWD,
            to_c.as_ptr(),
            sys::RENAME_NOREPLACE,
        )
    };
    // SAFETY: as above
    #[cfg(target_os = "macos")]
    let result = unsafe { sys::renamex_np(from_c.as_ptr(), to_c.as_ptr(), sys::RENAME_EXCL) };
    if result == 0 {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    const ENOSYS: i32 = 38;
    #[cfg(target_os = "macos")]
    const ENOSYS: i32 = 78;
    const EINVAL: i32 = 22;
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // the kernel or the filesystem has no exclusive rename: make the new
        // name with a hard link, which also refuses an existing name
        Some(ENOSYS | EINVAL) => {
            std::fs::hard_link(from, to)?;
            let _ = std::fs::remove_file(from);
            Ok(())
        }
        _ => Err(error),
    }
}

/// The path of a temporary file; the file is removed when the value is
/// dropped.
#[derive(Debug)]
pub struct TempPath {
    path: PathBuf,
    keep: bool,
}

impl TempPath {
    fn settle(mut self) -> PathBuf {
        self.keep = true;
        std::mem::take(&mut self.path)
    }

    /// Renames the file to `new_path`, replacing a file already there.
    pub fn persist(self, new_path: impl AsRef<Path>) -> Result<(), PathPersistError> {
        match std::fs::rename(&self.path, new_path) {
            Ok(()) => {
                self.settle();
                Ok(())
            }
            Err(error) => Err(PathPersistError { error, path: self }),
        }
    }

    /// Renames the file to `new_path`; if something is already there the
    /// error kind is `AlreadyExists` and nothing changes.
    pub fn persist_noclobber(self, new_path: impl AsRef<Path>) -> Result<(), PathPersistError> {
        match rename_exclusive(&self.path, new_path.as_ref()) {
            Ok(()) => {
                self.settle();
                Ok(())
            }
            Err(error) => Err(PathPersistError { error, path: self }),
        }
    }

    /// Gives up ownership: the file stays on disk and its path is returned.
    pub fn keep(self) -> Result<PathBuf, PathPersistError> {
        Ok(self.settle())
    }

    /// Removes the file now, reporting the error that a drop would ignore.
    pub fn close(self) -> io::Result<()> {
        std::fs::remove_file(self.settle())
    }
}

impl Deref for TempPath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// An open file with a name; the file is removed when the value is dropped
/// unless it was persisted or kept.
///
/// Created readable and writable by the owner only (mode 0600).
#[derive(Debug)]
pub struct NamedTempFile {
    path: TempPath,
    file: File,
}

impl NamedTempFile {
    /// Creates the file inside [`std::env::temp_dir`].
    pub fn new() -> io::Result<Self> {
        Self::new_in(std::env::temp_dir())
    }

    /// Creates the file inside `directory`.
    pub fn new_in(directory: impl AsRef<Path>) -> io::Result<Self> {
        create_unique(directory.as_ref(), OsStr::new(DEFAULT_PREFIX), |path| {
            let file = create_file(&path)?;
            Ok(Self {
                path: TempPath { path, keep: false },
                file,
            })
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn as_file(&self) -> &File {
        &self.file
    }

    pub fn as_file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Opens the file a second time, with its own position. Fails with
    /// `NotFound` if the name no longer refers to this file.
    pub fn reopen(&self) -> io::Result<File> {
        let reopened = OpenOptions::new().read(true).write(true).open(&self.path)?;
        let (old, new) = (self.file.metadata()?, reopened.metadata()?);
        if old.dev() != new.dev() || old.ino() != new.ino() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "original tempfile has been replaced",
            ));
        }
        Ok(reopened)
    }

    /// Renames the file to `new_path`, replacing a file already there, and
    /// returns the open file. On failure the error carries the temporary
    /// file back.
    pub fn persist(self, new_path: impl AsRef<Path>) -> Result<File, PersistError> {
        let Self { path, file } = self;
        match path.persist(new_path) {
            Ok(()) => Ok(file),
            Err(PathPersistError { error, path }) => Err(PersistError {
                error,
                file: Self { path, file },
            }),
        }
    }

    /// Renames the file to `new_path`; if something is already there the
    /// error kind is `AlreadyExists` and nothing changes.
    pub fn persist_noclobber(self, new_path: impl AsRef<Path>) -> Result<File, PersistError> {
        let Self { path, file } = self;
        match path.persist_noclobber(new_path) {
            Ok(()) => Ok(file),
            Err(PathPersistError { error, path }) => Err(PersistError {
                error,
                file: Self { path, file },
            }),
        }
    }

    /// Gives up ownership: the file stays on disk under its temporary name.
    pub fn keep(self) -> Result<(File, PathBuf), PersistError> {
        Ok((self.file, self.path.settle()))
    }

    /// Removes the name and returns the open file.
    pub fn into_file(self) -> File {
        self.file
    }

    /// Closes the file and returns its path, which still removes the file
    /// when dropped.
    pub fn into_temp_path(self) -> TempPath {
        self.path
    }

    pub fn into_parts(self) -> (File, TempPath) {
        (self.file, self.path)
    }

    /// Closes and removes the file now, reporting the error that a drop
    /// would ignore.
    pub fn close(self) -> io::Result<()> {
        self.path.close()
    }
}

impl Read for NamedTempFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.read(buffer)
    }
}

impl Write for NamedTempFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for NamedTempFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.file.seek(position)
    }
}

/// A failed [`NamedTempFile::persist`]: the cause and the file, still
/// temporary.
#[derive(Debug)]
pub struct PersistError {
    pub error: io::Error,
    pub file: NamedTempFile,
}

/// A failed [`TempPath::persist`]: the cause and the path, still temporary.
#[derive(Debug)]
pub struct PathPersistError {
    pub error: io::Error,
    pub path: TempPath,
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "failed to persist temporary file: {}",
            self.error
        )
    }
}

impl std::fmt::Display for PathPersistError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "failed to persist temporary file path: {}",
            self.error
        )
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl std::error::Error for PathPersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl From<PersistError> for io::Error {
    fn from(error: PersistError) -> Self {
        error.error
    }
}

impl From<PathPersistError> for io::Error {
    fn from(error: PathPersistError) -> Self {
        error.error
    }
}
