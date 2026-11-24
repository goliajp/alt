use super::*;

fn repository(at: &std::path::Path) {
    std::fs::create_dir_all(at).unwrap();
    alt_cli::native::init(Some(at.to_owned()), &mut Vec::new()).unwrap();
}

/// A repository is put aside while a request has it in hand, another is
/// served under its name, and the first is put back. The request goes on
/// with the writer it had, who asks meanwhile is given the other
/// repository with a writer of its own, and the one put back is written
/// through the writer it had: there are never two of one repository. A
/// sync that failed stays known to that repository when nobody has it in
/// hand, and is not taken for one of the other.
#[test]
fn a_repository_put_aside_and_put_back_is_written_through_the_one_writer_it_has() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let (served, aside) = (tmp.path().join("served"), tmp.path().join("aside"));
    repository(&served);
    let cache = Opened::of(4);
    let open = || opened(&cache, "served", served.clone(), None).unwrap();
    let writer = |handle: &RepoHandle| handle.writer.clone().unwrap();
    let put_aside = || {
        std::fs::rename(&served, &aside).unwrap();
        repository(&served);
    };
    let put_back = || {
        std::fs::remove_dir_all(&served).unwrap();
        std::fs::rename(&aside, &served).unwrap();
    };
    let in_hand = writer(&open());
    assert!(Arc::ptr_eq(&writer(&open()), &in_hand));

    put_aside();
    let other = writer(&open());
    assert!(!Arc::ptr_eq(&other, &in_hand));
    put_back();
    assert!(Arc::ptr_eq(&writer(&open()), &in_hand));

    // nobody has it in hand, and a sync of it failed
    in_hand.group.fail("no room".to_owned());
    drop(in_hand);
    put_aside();
    // a third repository, which is neither: the writer of the second is still
    // in hand, and its directory, kept open, is not taken for this one
    let third = writer(&open());
    assert!(!Arc::ptr_eq(&third, &other));
    assert_eq!(third.group.failed(), None);
    drop(third);
    put_back();
    assert_eq!(writer(&open()).group.failed().as_deref(), Some("no room"));
}

/// A repository that no request has in hand and whose syncs held is
/// closed for another, its writer with it.
#[test]
fn the_writer_of_a_repository_that_was_closed_is_not_kept() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let cache = Opened::of(1);
    let open = |name: &str| {
        let at = tmp.path().join(name);
        if !at.exists() {
            repository(&at);
        }
        opened(&cache, name, at, None).unwrap()
    };
    let first = Arc::downgrade(&open("first").writer.unwrap());
    assert!(first.upgrade().is_some());
    // the one repository that stays open is another now
    drop(open("second"));
    drop(open("second"));
    assert!(first.upgrade().is_none());
    assert_eq!(cache.take().repos.len(), 1);
}

/// A request has a repository in hand when it is put aside, and a sync of
/// it fails only then. The request ends, nobody has the writer in hand any
/// more, and the repository put back is still not written to.
#[test]
fn a_sync_that_failed_after_its_repository_was_put_aside_is_known_when_it_is_put_back() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let (served, aside) = (tmp.path().join("served"), tmp.path().join("aside"));
    repository(&served);
    let cache = Opened::of(4);
    let open = || opened(&cache, "served", served.clone(), None).unwrap();
    let in_hand = open().writer.unwrap();
    std::fs::rename(&served, &aside).unwrap();
    repository(&served);
    // another is served under the name, and the one put aside is closed
    assert!(!Arc::ptr_eq(&open().writer.unwrap(), &in_hand));
    in_hand.group.fail("no room".to_owned());
    // what a request does when it is done with its writer
    in_hand.remember();
    drop(in_hand);
    assert_eq!(open().writer.unwrap().group.failed(), None);
    std::fs::remove_dir_all(&served).unwrap();
    std::fs::rename(&aside, &served).unwrap();
    let back = open().writer.unwrap();
    assert_eq!(back.group.failed().as_deref(), Some("no room"));
}

/// A repository of a root is opened under the name the root lists it by.
/// Where the file system opens the directory under another writing of the
/// name too, that writing is no repository: it would have a writer of its
/// own beside the first, and the rules of a name that is not its own.
#[test]
fn a_repository_of_a_root_is_opened_under_the_name_the_root_lists_and_no_other() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path();
    repository(&root.join("secret"));
    let cache = Opened::of(4);
    let open = |name: &str| opened(&cache, name, root.join(name), Some(root));
    assert!(open("secret").is_ok());
    assert!(open("SECRET").is_err() && open("Secret").is_err());
    assert!(readers::listed(root, "secret").unwrap());
    assert!(!readers::listed(root, "SECRET").unwrap());
    assert_eq!(cache.take().repos.len(), 1);
}
