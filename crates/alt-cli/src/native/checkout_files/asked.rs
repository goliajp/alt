//! The directory a command asks the file system in which names are one
//! name: in `.alt` where that is a directory, and else in the working
//! tree itself, under a name that scans pass over and no path may have.

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

use alt_worktree::NAMES_ASKED;

use super::*;

/// A directory to ask the file system in which names are one name.
pub(super) struct Asked {
    dir: PathBuf,
}

impl Asked {
    /// An empty directory where names fold as they do in the tree at
    /// `root`; one left by a command cut short is cleared first.
    pub(super) fn at(root: &Path) -> Res<Self> {
        let control = root.join(".alt");
        let dir = match control.is_dir() {
            true => control.join("names-asked"),
            false => root.join(NAMES_ASKED),
        };
        if dir.symlink_metadata().is_ok() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir(&dir)?;
        Ok(Asked { dir })
    }

    /// `names` in groups of those that are one name here.
    pub(super) fn grouped<'n>(
        &self,
        names: impl Iterator<Item = &'n [u8]>,
    ) -> Res<Vec<Vec<&'n [u8]>>> {
        let mut groups: Vec<Vec<&[u8]>> = Vec::new();
        let mut of: HashMap<u64, usize> = HashMap::new();
        let mut made = Vec::new();
        for name in names {
            let path = self.dir.join(std::ffi::OsStr::from_bytes(name));
            let file = match path.symlink_metadata() {
                Ok(found) => found.ino(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let file = std::fs::File::create(&path)?.metadata()?.ino();
                    made.push(path);
                    file
                }
                Err(e) => return Err(e.into()),
            };
            let at = *of.entry(file).or_insert(groups.len());
            if at == groups.len() {
                groups.push(Vec::new());
            }
            groups[at].push(name);
        }
        crate::crash::point("names asked");
        for path in made {
            std::fs::remove_file(path)?;
        }
        Ok(groups)
    }
}

impl Drop for Asked {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Removes the directory a command cut short left in the working tree
/// at `root`.
pub(in crate::native) fn clear_left(root: &Path) -> Res<()> {
    match std::fs::remove_dir_all(root.join(NAMES_ASKED)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        done => Ok(done?),
    }
}

/// Whether names that differ in case are one name in the directory
/// `root`, which has no `.alt` yet to tell by.
pub(super) fn folds_in(root: &Path) -> Res<bool> {
    let asked = Asked::at(root)?;
    let name = asked
        .dir
        .file_name()
        .ok_or("the directory to ask in has no name")?;
    let other = name.as_bytes().to_ascii_uppercase();
    let other = asked
        .dir
        .with_file_name(std::ffi::OsStr::from_bytes(&other));
    Ok(other.symlink_metadata().is_ok())
}
