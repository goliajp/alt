//! Temporary files and directories.
//!
//! Every name is a prefix (`.tmp` unless a [`Builder`] sets one) followed by
//! twelve random letters and digits, and is created exclusively, so a name
//! that already exists is never reused.

use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

mod file;

pub use file::{NamedTempFile, PathPersistError, PersistError, TempPath, tempfile, tempfile_in};

const RANDOM_LENGTH: usize = 12;
const ATTEMPTS: usize = 1 << 16;
const DEFAULT_PREFIX: &str = ".tmp";

fn random_name(prefix: &OsStr) -> OsString {
    const ALPHABET: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut name = String::with_capacity(RANDOM_LENGTH);
    while name.len() < RANDOM_LENGTH {
        let mut bytes = [0u8; 2 * RANDOM_LENGTH];
        crate::random::fill(&mut bytes);
        // six bits per byte; values past the alphabet are dropped so that
        // every character is equally likely
        name.extend(
            bytes
                .iter()
                .map(|byte| usize::from(byte & 0x3f))
                .filter(|index| *index < ALPHABET.len())
                .take(RANDOM_LENGTH - name.len())
                .map(|index| char::from(ALPHABET[index])),
        );
    }
    let mut full = OsString::with_capacity(prefix.len() + RANDOM_LENGTH);
    full.push(prefix);
    full.push(name);
    full
}

/// Calls `create` with fresh random paths inside `directory` until one does
/// not exist yet.
fn create_unique<T>(
    directory: &Path,
    prefix: &OsStr,
    mut create: impl FnMut(PathBuf) -> io::Result<T>,
) -> io::Result<T> {
    // an absolute path stays valid for cleanup after the current directory
    // changes
    let directory = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        std::env::current_dir()?.join(directory)
    };
    for _ in 0..ATTEMPTS {
        let path = directory.join(random_name(prefix));
        match create(path.clone()) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("{error} at path {path:?}"),
                ));
            }
            Ok(value) => return Ok(value),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("too many temporary files exist at path {directory:?}"),
    ))
}

/// A directory that is removed, with everything in it, when the value is
/// dropped.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
    keep: bool,
}

/// Creates a temporary directory inside [`std::env::temp_dir`].
pub fn tempdir() -> io::Result<TempDir> {
    tempdir_in(std::env::temp_dir())
}

/// Creates a temporary directory inside `directory`.
pub fn tempdir_in(directory: impl AsRef<Path>) -> io::Result<TempDir> {
    Builder::new().tempdir_in(directory)
}

impl TempDir {
    /// Creates a temporary directory inside [`std::env::temp_dir`].
    pub fn new() -> io::Result<Self> {
        tempdir()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Gives up ownership: the directory stays on disk and its path is
    /// returned.
    pub fn keep(mut self) -> PathBuf {
        self.keep = true;
        std::mem::take(&mut self.path)
    }

    /// Removes the directory now, reporting the error that a drop would
    /// ignore.
    pub fn close(mut self) -> io::Result<()> {
        self.keep = true;
        std::fs::remove_dir_all(&self.path)
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Sets the name prefix of a temporary directory before creating it.
#[derive(Debug)]
pub struct Builder<'a> {
    prefix: &'a OsStr,
}

impl Default for Builder<'_> {
    fn default() -> Self {
        Self {
            prefix: OsStr::new(DEFAULT_PREFIX),
        }
    }
}

impl<'a> Builder<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the default `.tmp` at the start of the name.
    pub fn prefix<S: AsRef<OsStr> + ?Sized>(&mut self, prefix: &'a S) -> &mut Self {
        self.prefix = prefix.as_ref();
        self
    }

    /// Creates the directory inside `directory`, with the permissions a
    /// plain `mkdir` gives it.
    pub fn tempdir_in(&self, directory: impl AsRef<Path>) -> io::Result<TempDir> {
        create_unique(directory.as_ref(), self.prefix, |path| {
            std::fs::create_dir(&path)?;
            Ok(TempDir { path, keep: false })
        })
    }
}
