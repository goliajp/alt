//! Where `index_pack` puts a pack it renames and its idx, and what it
//! leaves where that fails.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use alt_git_codec::HashAlgo;
use alt_git_pack::index_pack;

use crate::index_pack::{plain_entry, written};

/// The names `index_pack` gives a pack it renames and its idx: those of
/// the trailer of the pack at `path`.
fn renamed(path: &Path) -> (PathBuf, PathBuf) {
    let bytes = std::fs::read(path).unwrap();
    let hex: String = bytes[bytes.len() - 20..]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = path.parent().unwrap();
    let named = |ext: &str| dir.join(format!("pack-{hex}.{ext}"));
    (named("pack"), named("idx"))
}

/// A pack whose idx cannot take its name leaves no idx behind, and is
/// itself under the name it came with
#[test]
fn a_pack_whose_idx_is_not_renamed_leaves_no_idx() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = written(&dir, &[plain_entry(b"one\n")]);
    let (pack, idx) = renamed(&path);
    // a directory that is not empty is not replaced by a file
    std::fs::create_dir(&idx).unwrap();
    std::fs::write(idx.join("in-the-way"), "").unwrap();
    index_pack(&path, HashAlgo::Sha1, true).unwrap_err();
    assert!(path.exists());
    assert!(!pack.exists());
    assert!(!path.with_extension("idx").exists());
    assert!(!path.with_extension("idx.tmp").exists());
}

/// A pack that cannot take its name leaves no idx behind
#[test]
fn a_pack_that_is_not_renamed_leaves_no_idx() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = written(&dir, &[plain_entry(b"one\n")]);
    let (pack, idx) = renamed(&path);
    std::fs::create_dir(&pack).unwrap();
    std::fs::write(pack.join("in-the-way"), "").unwrap();
    index_pack(&path, HashAlgo::Sha1, true).unwrap_err();
    assert!(path.exists());
    assert!(!idx.exists());
    assert!(!path.with_extension("idx").exists());
    assert!(!path.with_extension("idx.tmp").exists());
}

/// The inode of the file at `path`.
fn inode(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).unwrap().ino()
}

/// A pack whose names a pack and an idx have already, which are the same
/// as Git takes them to be, leaves those as they are and is removed
#[test]
fn a_pack_whose_names_are_taken_leaves_what_has_them() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = written(&dir, &[plain_entry(b"one\n")]);
    let (pack, idx) = renamed(&path);
    index_pack(&path, HashAlgo::Sha1, true).unwrap();
    let had = (inode(&pack), inode(&idx));

    let path = written(&dir, &[plain_entry(b"one\n")]);
    let indexed = index_pack(&path, HashAlgo::Sha1, true).unwrap();
    assert_eq!((&indexed.pack_path, &indexed.idx_path), (&pack, &idx));
    assert_eq!((inode(&pack), inode(&idx)), had);
    assert!(!path.exists());
    assert!(!path.with_extension("idx").exists());
}

/// A pack whose name a pack has already, though no idx has its own, has
/// its idx put beside that pack, which stays as it is
#[test]
fn a_pack_whose_name_alone_is_taken_has_its_idx_put_beside_what_has_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = written(&dir, &[plain_entry(b"one\n")]);
    let (pack, idx) = renamed(&path);
    index_pack(&path, HashAlgo::Sha1, true).unwrap();
    std::fs::remove_file(&idx).unwrap();
    let had = inode(&pack);

    let path = written(&dir, &[plain_entry(b"one\n")]);
    index_pack(&path, HashAlgo::Sha1, true).unwrap();
    assert_eq!(inode(&pack), had);
    assert!(idx.is_file());
    assert!(!path.exists());
}

/// A pack whose name a pack has already, and whose idx cannot take its
/// own, leaves the pack that has the name where it is
#[test]
fn a_pack_whose_idx_is_not_renamed_leaves_the_pack_that_has_its_name() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = written(&dir, &[plain_entry(b"one\n")]);
    let (pack, idx) = renamed(&path);
    index_pack(&path, HashAlgo::Sha1, true).unwrap();
    std::fs::remove_file(&idx).unwrap();
    std::fs::create_dir(&idx).unwrap();
    std::fs::write(idx.join("in-the-way"), "").unwrap();
    let had = inode(&pack);

    let path = written(&dir, &[plain_entry(b"one\n")]);
    index_pack(&path, HashAlgo::Sha1, true).unwrap_err();
    assert_eq!(inode(&pack), had);
    assert!(!path.with_extension("idx").exists());
    assert!(!path.with_extension("idx.tmp").exists());
}
