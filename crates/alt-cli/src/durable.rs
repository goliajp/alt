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

/// Puts `bytes` at `path` in one step and syncs the file and its entry in
/// the directory it is in, and no directory above that one: for a
/// directory whose own entry the caller has synced.
pub fn write_entry(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let mut tmp = alt_os::temp::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()
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
