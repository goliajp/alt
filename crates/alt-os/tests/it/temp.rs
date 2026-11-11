use alt_os::temp::{self, NamedTempFile};
use std::{
    io::{self, Read, Seek, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn directory_is_removed_with_its_contents_on_drop() {
    let directory = temp::tempdir().unwrap();
    let path = directory.path().to_owned();
    assert!(path.is_absolute() && path.is_dir());
    assert!(path.starts_with(std::env::temp_dir()));
    std::fs::create_dir_all(path.join("a/b")).unwrap();
    std::fs::write(path.join("a/b/file"), b"x").unwrap();
    drop(directory);
    assert!(!path.exists());
}

#[test]
fn directory_names_are_random_and_carry_the_prefix() {
    let parent = temp::tempdir().unwrap();
    let first = temp::tempdir_in(parent.path()).unwrap();
    let second = temp::tempdir_in(parent.path()).unwrap();
    let named = temp::Builder::new()
        .prefix("stage-")
        .tempdir_in(parent.path())
        .unwrap();
    assert_ne!(first.path(), second.path());
    for directory in [&first, &second] {
        let name = directory.path().file_name().unwrap().to_str().unwrap();
        let random = name.strip_prefix(".tmp").unwrap();
        assert_eq!(random.len(), 12);
        assert!(random.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    }
    let name = named.path().file_name().unwrap().to_str().unwrap();
    assert_eq!(name.strip_prefix("stage-").unwrap().len(), 12);
    assert_eq!(first.path().parent().unwrap(), parent.path());
    assert_eq!(entries(parent.path()).len(), 3);
}

#[test]
fn directory_has_the_permissions_of_a_plain_mkdir() {
    let parent = temp::tempdir().unwrap();
    std::fs::create_dir(parent.path().join("plain")).unwrap();
    let directory = temp::tempdir_in(parent.path()).unwrap();
    assert_eq!(mode(directory.path()), mode(&parent.path().join("plain")));
}

#[test]
fn kept_directory_stays_and_closed_directory_goes() {
    let parent = temp::tempdir().unwrap();
    let kept = temp::tempdir_in(parent.path()).unwrap().keep();
    assert!(kept.is_dir());
    let closed = temp::tempdir_in(parent.path()).unwrap();
    let path = closed.path().to_owned();
    closed.close().unwrap();
    assert!(!path.exists());
    assert!(kept.is_dir());
}

#[test]
fn relative_parent_gives_an_absolute_path() {
    let directory = temp::tempdir_in(".").unwrap();
    assert!(directory.path().is_absolute());
    assert_eq!(
        directory.path().parent().unwrap(),
        std::env::current_dir().unwrap()
    );
    let file = NamedTempFile::new_in(".").unwrap();
    assert!(file.path().is_absolute());
}

#[test]
fn missing_parent_is_reported_as_not_found() {
    let parent = temp::tempdir().unwrap();
    let missing = parent.path().join("missing");
    assert_eq!(
        temp::tempdir_in(&missing).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        NamedTempFile::new_in(&missing).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        temp::tempfile_in(&missing).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn named_file_is_private_and_removed_on_drop() {
    let parent = temp::tempdir().unwrap();
    let mut file = NamedTempFile::new_in(parent.path()).unwrap();
    let path = file.path().to_owned();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(path.parent().unwrap(), parent.path());
    file.write_all(b"hello").unwrap();
    file.rewind().unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "hello");
    assert_eq!(file.as_file().metadata().unwrap().len(), 5);
    drop(file);
    assert!(!path.exists());

    let default = NamedTempFile::new().unwrap();
    assert!(default.path().starts_with(std::env::temp_dir()));
}

#[test]
fn reopen_sees_writes_and_detects_a_replaced_file() {
    let mut file = NamedTempFile::new().unwrap();
    file.as_file_mut().write_all(b"written first").unwrap();
    let mut reopened = file.reopen().unwrap();
    let mut text = String::new();
    reopened.read_to_string(&mut text).unwrap();
    assert_eq!(text, "written first");
    reopened.write_all(b", then more").unwrap();
    assert_eq!(
        std::fs::read(file.path()).unwrap(),
        b"written first, then more"
    );

    std::fs::remove_file(file.path()).unwrap();
    std::fs::write(file.path(), b"other").unwrap();
    assert_eq!(file.reopen().unwrap_err().kind(), io::ErrorKind::NotFound);
}

#[test]
fn persist_renames_and_replaces() {
    let parent = temp::tempdir().unwrap();
    let target = parent.path().join("target");
    std::fs::write(&target, b"old").unwrap();
    let mut file = NamedTempFile::new_in(parent.path()).unwrap();
    file.write_all(b"new").unwrap();
    let inode = file.as_file().metadata().unwrap().ino();
    let mut persisted = file.persist(&target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert_eq!(std::fs::metadata(&target).unwrap().ino(), inode);
    assert_eq!(entries(parent.path()), ["target"]);
    // the returned handle is the same open file
    persisted.write_all(b"er").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"newer");
}

#[test]
fn persist_noclobber_refuses_an_existing_name_and_returns_the_file() {
    let parent = temp::tempdir().unwrap();
    let target = parent.path().join("target");
    let mut first = NamedTempFile::new_in(parent.path()).unwrap();
    first.write_all(b"first").unwrap();
    first.persist_noclobber(&target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"first");

    let mut second = NamedTempFile::new_in(parent.path()).unwrap();
    second.write_all(b"second").unwrap();
    let failure = second.persist_noclobber(&target).unwrap_err();
    assert_eq!(failure.error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&target).unwrap(), b"first");
    // the temporary file is handed back intact and still temporary
    let path = failure.file.path().to_owned();
    assert_eq!(std::fs::read(&path).unwrap(), b"second");
    let error: io::Error = failure.into();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(!path.exists());
    assert_eq!(entries(parent.path()), ["target"]);
}

#[test]
fn failed_persist_keeps_the_temporary_file() {
    let parent = temp::tempdir().unwrap();
    let file = NamedTempFile::new_in(parent.path()).unwrap();
    let failure = file
        .persist(parent.path().join("missing/target"))
        .unwrap_err();
    assert_eq!(failure.error.kind(), io::ErrorKind::NotFound);
    assert!(failure.file.path().exists());
}

#[test]
fn keep_leaves_the_file_under_its_temporary_name() {
    let parent = temp::tempdir().unwrap();
    let mut file = NamedTempFile::new_in(parent.path()).unwrap();
    file.write_all(b"kept").unwrap();
    let (_, path) = file.keep().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"kept");
    assert_eq!(path.parent().unwrap(), parent.path());
}

#[test]
fn parts_of_a_named_file_have_separate_lifetimes() {
    let parent = temp::tempdir().unwrap();

    // into_file removes the name; the handle keeps working
    let named = NamedTempFile::new_in(parent.path()).unwrap();
    let path = named.path().to_owned();
    let mut file = named.into_file();
    assert!(!path.exists());
    file.write_all(b"unnamed").unwrap();
    assert_eq!(file.metadata().unwrap().len(), 7);

    // into_temp_path closes the handle; the path removes the file on drop
    let named = NamedTempFile::new_in(parent.path()).unwrap();
    let temp_path = named.into_temp_path();
    let path = temp_path.to_path_buf();
    assert!(path.exists());
    drop(temp_path);
    assert!(!path.exists());

    // a path can be persisted without the handle
    let (mut file, temp_path) = NamedTempFile::new_in(parent.path()).unwrap().into_parts();
    file.write_all(b"parts").unwrap();
    let target = parent.path().join("target");
    temp_path.persist_noclobber(&target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"parts");
    let (_, temp_path) = NamedTempFile::new_in(parent.path()).unwrap().into_parts();
    let failure = temp_path.persist_noclobber(&target).unwrap_err();
    assert_eq!(failure.error.kind(), io::ErrorKind::AlreadyExists);
    drop(failure);

    // close removes the file and reports success
    let named = NamedTempFile::new_in(parent.path()).unwrap();
    let path = named.path().to_owned();
    named.close().unwrap();
    assert!(!path.exists());
    assert_eq!(entries(parent.path()), ["target"]);
}

#[test]
fn unnamed_file_has_no_directory_entry() {
    let parent = temp::tempdir().unwrap();
    let mut file = temp::tempfile_in(parent.path()).unwrap();
    assert!(entries(parent.path()).is_empty());
    assert_eq!(file.metadata().unwrap().nlink(), 0);
    assert_eq!(file.metadata().unwrap().mode() & 0o7777, 0o600);
    file.write_all(b"scratch").unwrap();
    file.rewind().unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "scratch");

    let mut default = temp::tempfile().unwrap();
    default.write_all(b"x").unwrap();
    assert_eq!(default.metadata().unwrap().nlink(), 0);
}
