//! The index file as it is written, and what no index may hold.

use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_git_index::Index;

use super::Res;

/// Refuses an index in which a path is a file and a directory at once: no
/// command may bring one about, so this is an error of the program.
pub(super) fn ensure_sound(index: &Index) -> Res<()> {
    match index.clash() {
        Some(path) => Err(format!("index would hold '{path}' as a file and as a directory").into()),
        None => Ok(()),
    }
}

pub(super) fn empty_index() -> Index {
    Index {
        version: 2,
        entries: Vec::new(),
        extensions: Vec::new(),
    }
}

/// Atomic index write to `path`: temp file + rename (sibling temp).
///
/// Racily clean entries are handled as git does: an entry whose mtime is not
/// older than the index file itself may be rewritten within the same clock
/// tick (coarse on Linux) with the same size, which a stat compare cannot
/// see. Those entries get a zero mtime, so the next scan re-hashes them.
pub(super) fn save_index(path: &Path, index: &Index, algo: HashAlgo) -> Res<()> {
    ensure_sound(index)?;
    let tmp = path.with_extension("tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::durable::write(&tmp, index.serialize(algo))?;
    let stamp = file_mtime(&std::fs::metadata(&tmp)?);
    if index.entries.iter().any(|e| e.mtime >= stamp) {
        let mut smudged = index.clone();
        for e in smudged.entries.iter_mut().filter(|e| e.mtime >= stamp) {
            e.mtime = (0, 0);
        }
        crate::durable::write(&tmp, smudged.serialize(algo))?;
    }
    crate::durable::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
fn file_mtime(meta: &std::fs::Metadata) -> (u32, u32) {
    use std::os::unix::fs::MetadataExt;
    (meta.mtime() as u32, meta.mtime_nsec() as u32)
}

// stat is never trusted off unix, so no entry is ever racily clean
#[cfg(not(unix))]
fn file_mtime(_meta: &std::fs::Metadata) -> (u32, u32) {
    (u32::MAX, u32::MAX)
}

#[cfg(test)]
mod tests {
    use alt_git_codec::{ObjectId, ObjectKind};
    use alt_git_index::IndexEntry;

    use super::*;

    fn index(paths: &[&str]) -> Index {
        let entry = |path: &&str| IndexEntry {
            ctime: (0, 0),
            mtime: (0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            size: 0,
            oid: ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b""),
            flags: path.len() as u16,
            extended_flags: None,
            path: (*path).into(),
        };
        Index {
            entries: paths.iter().map(entry).collect(),
            ..empty_index()
        }
    }

    #[test]
    fn an_index_with_a_path_that_is_a_file_and_a_directory_is_not_written() {
        let dir = alt_os::temp::tempdir().unwrap();
        let at = dir.path().join("index");
        for clashing in [&["a", "a.txt", "a/b"][..], &["d/e", "d/e-f", "d/e/f/g"]] {
            let refused = save_index(&at, &index(clashing), HashAlgo::Sha1).unwrap_err();
            let named = format!("index would hold '{}' as a file and", clashing[0]);
            assert!(refused.to_string().starts_with(&named), "{refused}");
            assert!(!at.exists() && !at.with_extension("tmp").exists());
        }
        save_index(&at, &index(&["a-b", "a.txt", "a/b"]), HashAlgo::Sha1).unwrap();
        assert!(at.exists());
    }
}
