//! Filesystem state writes used before publishing a command receipt.
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

fn sync_parents(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        let parent = fs::canonicalize(if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        })?;
        for directory in parent.ancestors() {
            File::open(directory)?.sync_all()?;
        }
    }
    Ok(())
}

pub fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = alt_os::temp::NamedTempFile::new_in(parent)?;
    // Replacement preserves an existing regular file's mode. A new state
    // file remains private (0600); worktree materialization sets its mode.
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.is_file()
    {
        tmp.as_file().set_permissions(metadata.permissions())?;
    }
    tmp.write_all(bytes.as_ref())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|error| error.error)?;
    sync_parents(path)
}

pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    fs::remove_file(path)?;
    sync_parents(path)
}

pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let from = from.as_ref();
    let to = to.as_ref();
    fs::rename(from, to)?;
    let metadata = fs::symlink_metadata(to)?;
    if metadata.is_file() {
        File::open(to)?.sync_all()?;
    }
    sync_parents(from)?;
    sync_parents(to)
}

pub fn set_permissions(path: impl AsRef<Path>, permissions: fs::Permissions) -> io::Result<()> {
    let path = path.as_ref();
    fs::set_permissions(path, permissions)?;
    File::open(path)?.sync_all()
}

pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    fs::remove_dir_all(path)?;
    sync_parents(path)
}
