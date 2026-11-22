//! Filesystem state writes used before publishing a command receipt.
//!
//! Each is synced before it returns, unless the thread runs a command that
//! leaves what it writes unsynced ([`alt_os::durability`]): the write is
//! made the same way then, and no file or directory is synced.
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// Syncs `file`, where the running thread syncs what it writes.
pub fn sync(file: &File) -> io::Result<()> {
    match alt_os::durability::unsynced() {
        true => Ok(()),
        false => file.sync_all(),
    }
}

/// [`sync`] of the file or directory at `path`, opened only to be synced.
pub fn sync_at(path: &Path) -> io::Result<()> {
    match alt_os::durability::unsynced() {
        true => Ok(()),
        false => File::open(path)?.sync_all(),
    }
}

fn sync_parents(path: &Path) -> io::Result<()> {
    if alt_os::durability::unsynced() {
        return Ok(());
    }
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
    sync(tmp.as_file())?;
    tmp.persist(path).map_err(|error| error.error)?;
    sync_parents(path)
}

/// Puts `bytes` at `path` in one step, making the directory it is in where
/// that is missing, and syncs the file, that directory and the two above
/// it: state of a workspace or of the recovery of one, whose directories
/// may be as new as the file.
pub fn write_state(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(parent)?;
    let mut tmp = alt_os::temp::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    sync(tmp.as_file())?;
    tmp.persist(path).map_err(|error| error.error)?;
    if alt_os::durability::unsynced() {
        return Ok(());
    }
    // the directory as it is named in full: the parent of a relative path
    // of one component is the empty path, which cannot be opened
    for directory in fs::canonicalize(parent)?.ancestors().take(3) {
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

/// Puts the file at `from` at `path` as well, in one step: one file under
/// two names, for state that is kept twice with the same bytes. `from` is
/// synced first unless `synced` says its content was already, then the
/// directories [`write_state`] syncs. Where the file system makes no such
/// link nothing is done and `false` is returned.
///
/// The link is first made as `<path>.next`: the caller holds the lock
/// that keeps every other writer of `path` out.
pub fn link_state(from: &Path, path: &Path, synced: bool) -> io::Result<bool> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let next = path.with_extension("next");
    match fs::hard_link(from, &next) {
        Ok(()) => {}
        // left by a writer that was cut short
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(&next)?;
            fs::hard_link(from, &next)?;
        }
        Err(_) => return Ok(false),
    }
    if !synced {
        sync_at(from)?;
    }
    fs::rename(&next, path)?;
    if alt_os::durability::unsynced() {
        return Ok(true);
    }
    for directory in fs::canonicalize(parent)?.ancestors().take(3) {
        File::open(directory)?.sync_all()?;
    }
    Ok(true)
}

/// Puts `bytes` at `path` in one step and syncs the file and its entry in
/// the directory it is in, and no directory above that one: for a
/// directory whose own entry the caller has synced.
pub fn write_entry(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let mut tmp = alt_os::temp::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    sync(tmp.as_file())?;
    tmp.persist(path).map_err(|error| error.error)?;
    sync_at(parent)
}

/// Whether the file at `path` holds `bytes` and nothing else; `None` where
/// there is no such file. The file is read a piece at a time: a large one
/// is not held a second time to be compared.
pub fn holds(path: &Path, mut bytes: &[u8]) -> io::Result<Option<bool>> {
    use std::io::Read;
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut piece = [0u8; 1 << 16];
    loop {
        let read = match file.read(&mut piece) {
            Ok(0) => return Ok(Some(bytes.is_empty())),
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        match bytes.split_at_checked(read) {
            Some((same, rest)) if same == &piece[..read] => bytes = rest,
            _ => return Ok(Some(false)),
        }
    }
}

/// Syncs the `levels` directories above `directory`: the entry of a
/// directory that was just made, and the entries of those it lies in. Once
/// that is done, what is put into the directory needs [`write_entry`] only.
pub fn sync_above(directory: &Path, levels: usize) -> io::Result<()> {
    if alt_os::durability::unsynced() {
        return Ok(());
    }
    for above in fs::canonicalize(directory)?
        .ancestors()
        .skip(1)
        .take(levels)
    {
        File::open(above)?.sync_all()?;
    }
    Ok(())
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
    if metadata.is_file() && !alt_os::durability::unsynced() {
        File::open(to)?.sync_all()?;
    }
    sync_parents(from)?;
    sync_parents(to)
}

pub fn set_permissions(path: impl AsRef<Path>, permissions: fs::Permissions) -> io::Result<()> {
    let path = path.as_ref();
    fs::set_permissions(path, permissions)?;
    sync_at(path)
}

pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    fs::remove_dir_all(path)?;
    sync_parents(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_holds_bytes_only_when_it_is_exactly_them() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("file");
        // longer than a piece, and not a whole number of them
        let bytes: Vec<u8> = (0..200_003u32).map(|n| (n % 251) as u8).collect();
        assert_eq!(holds(&path, &bytes).unwrap(), None);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(holds(&path, &bytes).unwrap(), Some(true));
        // shorter, longer, and one byte apart in the last piece
        assert_eq!(
            holds(&path, &bytes[..bytes.len() - 1]).unwrap(),
            Some(false)
        );
        assert_eq!(
            holds(&path, &[&bytes[..], &[0]].concat()).unwrap(),
            Some(false)
        );
        let mut other = bytes.clone();
        *other.last_mut().unwrap() ^= 1;
        assert_eq!(holds(&path, &other).unwrap(), Some(false));
        fs::write(&path, b"").unwrap();
        assert_eq!(holds(&path, b"").unwrap(), Some(true));
        assert_eq!(holds(&path, &bytes).unwrap(), Some(false));
    }
}
