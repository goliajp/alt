//! `alt add` leaves what it writes unsynced by default, the next command
//! that syncs makes it durable, and `core.fsync` or a request id of the
//! caller's own has the add sync as well.

use std::fs;
use std::path::Path;

use super::log_tails::{odb_flags, oplog_flags};
use super::sync_order::{FILES, lens, log_len, log_synced, objects_synced};
use super::unconfirmed::{alt, ok, stderr};

/// One commit, made by commands that sync.
pub(super) fn repo(dir: &Path) {
    ok(alt(dir, &["init"]));
    ok(alt(dir, &["config", "core.fsync", "added"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    ok(alt(dir, &["config", "--unset", "core.fsync"]));
}

pub(super) fn add(dir: &Path, name: &str) {
    fs::write(dir.join(name), format!("{name}\n")).unwrap();
    ok(alt(dir, &["add", name]));
}

/// Whether the markers record every file as synced to its end, with no
/// unsynced appends to follow.
fn synced_to_the_ends(dir: &Path) -> bool {
    let alt_dir = dir.join(".alt");
    (odb_flags(&alt_dir), oplog_flags(&alt_dir)) == (0, 0)
        && objects_synced(&alt_dir) == lens(&alt_dir)
        && log_synced(&alt_dir) == log_len(&alt_dir)
}

/// What a power cut leaves of the files the markers describe: each ends
/// where its marker says it was synced.
pub(super) fn power_cut(dir: &Path) {
    let alt_dir = dir.join(".alt");
    let cut = |name: &str, len: u64| {
        let file = fs::OpenOptions::new().write(true).open(alt_dir.join(name));
        file.unwrap().set_len(len).unwrap();
    };
    for (name, len) in FILES.iter().zip(objects_synced(&alt_dir)) {
        cut(name, len);
    }
    cut("oplog/log", log_synced(&alt_dir));
}

#[test]
fn a_commit_made_from_adds_that_were_not_synced_survives_a_power_cut() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    assert!(synced_to_the_ends(dir));
    add(dir, "b.txt");
    add(dir, "c.txt");
    // the adds were not synced, and the markers say that such appends follow
    let alt_dir = dir.join(".alt");
    assert_eq!((odb_flags(&alt_dir), oplog_flags(&alt_dir)), (1, 1));
    assert!(log_synced(&alt_dir) < log_len(&alt_dir));

    ok(alt(dir, &["commit", "-m", "second"]));
    assert!(synced_to_the_ends(dir));
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    power_cut(dir);
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
    let commit = ok(alt(dir, &["cat-file", "-p", "HEAD"]));
    let tree = commit.lines().next().unwrap().strip_prefix("tree ");
    let tree = ok(alt(dir, &["cat-file", "-p", tree.unwrap()]));
    assert!(tree.contains("b.txt") && tree.contains("c.txt"), "{tree}");
    let status = ok(alt(dir, &["status"]));
    assert!(status.contains("nothing to commit"), "{status}");
    let check = alt(dir, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{}", stderr(&check));
    assert!(ok(check).ends_with("every record verified\n"));
}

#[test]
fn an_add_that_was_not_synced_and_is_lost_to_a_power_cut_leaves_the_commit_before_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    let index = fs::read(dir.join(".alt/index")).unwrap();
    add(dir, "b.txt");
    // nothing the add wrote reached the disk
    power_cut(dir);
    crate::replace_file(&dir.join(".alt/index"), &index);
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
    let status = ok(alt(dir, &["status"]));
    assert!(status.contains("b.txt"), "{status}");
    add(dir, "b.txt");
    ok(alt(dir, &["commit", "-m", "second"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with("every record verified\n"));
}

#[cfg(target_os = "linux")]
mod files {
    use super::super::synced_files::{library, synced};
    use super::super::unconfirmed::alt_command;
    use super::*;

    const INDEX: &str = ".alt/index";
    const LOG: &str = ".alt/oplog/log";
    const PACK: &str = ".alt/store/pack-00000001.altpack";

    fn snapshots(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir.join(".alt/recovery/objects"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn count(synced: &[String], path: &str) -> usize {
        synced.iter().filter(|synced| *synced == path).count()
    }

    #[test]
    fn an_add_syncs_no_file_and_the_next_commit_syncs_what_the_adds_wrote() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = &tmp.path().join("repo");
        fs::create_dir(dir).unwrap();
        repo(dir);
        let library = library(tmp.path());
        let before = snapshots(dir);
        let run = |name: &str| {
            fs::write(dir.join(name), format!("{name}\n")).unwrap();
            let (out, synced) = synced(dir, &library, alt_command(dir, &["add", name]));
            ok(out);
            synced
        };
        // the first add after a command that syncs has the two markers say
        // that unsynced appends follow, and the second takes back the note
        // of a synced object store; no file an add writes is synced
        let told = [".alt/odb.durable", ".alt/oplog/durable", ".alt/odb.lock"];
        for name in ["b.txt", "c.txt"] {
            let synced = run(name);
            assert!(
                synced.iter().all(|path| told.contains(&&**path)),
                "{synced:?}"
            );
        }
        assert_eq!(run("d.txt"), Vec::<String>::new());

        let (out, synced) = synced(dir, &library, alt_command(dir, &["commit", "-m", "second"]));
        ok(out);
        for path in [INDEX, LOG, PACK, ".alt/map.alt", ".alt/recovery/objects"] {
            assert!(count(&synced, path) > 0, "{path} of {synced:?}");
        }
        let written = snapshots(dir);
        let written: Vec<_> = written
            .iter()
            .filter(|name| !before.contains(name))
            .collect();
        assert_eq!(written.len(), 3, "the index after each add");
        for name in written {
            let path = format!(".alt/recovery/objects/{name}");
            assert!(count(&synced, &path) > 0, "{path} of {synced:?}");
        }
        assert!(synced_to_the_ends(dir));
    }

    #[test]
    fn an_add_syncs_where_core_fsync_has_the_index_in_any_configuration() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = &tmp.path().join("repo");
        fs::create_dir(dir).unwrap();
        repo(dir);
        let library = library(tmp.path());
        let user = tmp.path().join("gitconfig");
        let run = |name: &str, fsync: Option<&str>| {
            fs::write(dir.join(name), format!("{name}\n")).unwrap();
            let mut command = alt_command(dir, &["add", name]);
            if let Some(fsync) = fsync {
                fs::write(&user, format!("[core]\n\tfsync = {fsync}\n")).unwrap();
                command.env("GIT_CONFIG_GLOBAL", &user);
            }
            let (out, synced) = synced(dir, &library, command);
            (stderr(&out), ok(out), synced).2
        };
        // the user's configuration
        for (name, fsync) in [("b", "added"), ("c", "committed,index"), ("d", "all")] {
            let synced = run(name, Some(fsync));
            for path in [LOG, PACK, ".alt/recovery/objects", ".alt"] {
                assert!(count(&synced, path) > 0, "{fsync}: {path} of {synced:?}");
            }
            assert!(synced_to_the_ends(dir), "{fsync}");
        }
        for (name, fsync) in [("e", "committed"), ("f", "none"), ("g", "added,-index")] {
            let synced = run(name, Some(fsync));
            assert_eq!(
                count(&synced, LOG) + count(&synced, PACK),
                0,
                "{fsync}: {synced:?}"
            );
            assert!(!synced_to_the_ends(dir), "{fsync}");
        }
        // the repository's own, which is read after the user's
        ok(alt(dir, &["config", "core.fsync", "added"]));
        assert!(count(&run("h", Some("committed")), LOG) > 0);
        assert!(synced_to_the_ends(dir));
        ok(alt(dir, &["config", "core.fsync", "committed"]));
        assert_eq!(count(&run("i", Some("added")), LOG), 0);
    }

    #[test]
    fn a_name_core_fsync_does_not_know_is_told_once_and_changes_nothing() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = &tmp.path().join("repo");
        fs::create_dir(dir).unwrap();
        repo(dir);
        let library = library(tmp.path());
        ok(alt(dir, &["config", "core.fsync", "added,Index"]));
        fs::write(dir.join("b.txt"), "b\n").unwrap();
        let (out, synced) = synced(dir, &library, alt_command(dir, &["add", "b.txt"]));
        let told = "warning: ignoring unknown core.fsync component 'Index'\n";
        assert_eq!(stderr(&out), told);
        ok(out);
        assert!(count(&synced, LOG) > 0, "{synced:?}");
    }

    /// `b.txt` added under a request id the caller chose, in a repository
    /// of one commit: the id, and the files the add synced.
    fn added_under_an_id_of_the_callers(tmp: &Path) -> (String, Vec<String>) {
        let dir = &tmp.join("repo");
        fs::create_dir(dir).unwrap();
        repo(dir);
        let library = library(tmp);
        let id = "0123456789abcdef0123456789abcdef";
        fs::write(dir.join("b.txt"), "b\n").unwrap();
        let add = alt_command(dir, &["--request-id", id, "add", "b.txt"]);
        let (out, synced) = synced(dir, &library, add);
        ok(out);
        (id.to_owned(), synced)
    }

    #[test]
    fn an_add_under_a_request_id_of_the_callers_own_is_synced_with_its_receipt() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let (id, synced) = added_under_an_id_of_the_callers(tmp.path());
        let dir = &tmp.path().join("repo");
        assert!(synced_to_the_ends(dir));
        // the index is synced under the name it is written by
        for path in [LOG, PACK, ".alt", &format!(".alt/requests/{id}")] {
            assert!(count(&synced, path) > 0, "{path} of {synced:?}");
        }
        let status = ok(alt(dir, &["request", "status", &id]));
        assert!(status.contains("\"state\": \"completed\""), "{status}");
    }

    #[test]
    fn the_receipts_of_a_request_are_synced_with_their_directory_and_those_above_it_once() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let (id, synced) = added_under_an_id_of_the_callers(tmp.path());
        // the request's directory is synced into the two above it when it
        // is made; the intent, the response and the completion are each
        // synced with the directory they are put in, and nothing above it
        let request = format!(".alt/requests/{id}");
        assert_eq!(count(&synced, ".alt/requests"), 1, "{synced:?}");
        assert_eq!(count(&synced, &request), 3, "{synced:?}");
        let within = |path: &&String| path.starts_with(&format!("{request}/"));
        assert_eq!(synced.iter().filter(within).count(), 3, "{synced:?}");
        // the record that the request was submitted, with its directory
        let submitted = |path: &&String| path.starts_with(".alt/submissions/");
        assert_eq!(synced.iter().filter(submitted).count(), 1, "{synced:?}");
        assert_eq!(count(&synced, ".alt/submissions"), 1, "{synced:?}");
    }

    #[test]
    fn the_link_to_lfs_content_is_synced_by_the_add_that_stores_it() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = &tmp.path().join("repo");
        fs::create_dir(dir).unwrap();
        repo(dir);
        let library = library(tmp.path());
        let attributes = "*.bin filter=lfs diff=lfs merge=lfs -text\n";
        fs::write(dir.join(".gitattributes"), attributes).unwrap();
        fs::write(dir.join("w.bin"), vec![9u8; 50_000]).unwrap();
        let (out, synced) = synced(dir, &library, alt_command(dir, &["add", "w.bin"]));
        ok(out);
        // no op names the link: nothing would sync it later, and a commit
        // of the pointer must find the content after a power cut
        let link = |path: &String| path.starts_with(".alt/lfs/");
        assert!(synced.iter().any(link), "{synced:?}");
        assert!(count(&synced, PACK) > 0, "{synced:?}");
        assert_eq!(count(&synced, LOG) + count(&synced, INDEX), 0, "{synced:?}");
    }

    #[test]
    fn a_daemon_syncs_no_file_for_an_add_and_syncs_what_the_adds_wrote_for_a_commit() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = &tmp.path().join("repo");
        fs::create_dir(dir).unwrap();
        repo(dir);
        let library = library(tmp.path());
        let served = |args: &[&str]| {
            let mut command = alt_command(dir, args);
            command.env_remove("ALT_NO_DAEMON");
            command.env("ALT_DAEMON_IDLE_MS", "6000");
            let (out, synced) = synced(dir, &library, command);
            ok(out);
            synced
        };
        let told = [".alt/odb.durable", ".alt/oplog/durable", ".alt/odb.lock"];
        fs::write(dir.join("b.txt"), "b\n").unwrap();
        let first = served(&["add", "b.txt"]);
        assert!(
            first.iter().all(|path| told.contains(&&**path)),
            "{first:?}"
        );
        let meta = fs::read(dir.join(".alt/daemon.meta")).unwrap();
        fs::write(dir.join("c.txt"), "c\n").unwrap();
        assert_eq!(served(&["add", "c.txt"]), Vec::<String>::new());
        let synced = served(&["commit", "-m", "second"]);
        for path in [INDEX, LOG, PACK, ".alt/recovery/objects"] {
            assert!(count(&synced, path) > 0, "{path} of {synced:?}");
        }
        assert!(synced_to_the_ends(dir));
        assert_eq!(fs::read(dir.join(".alt/daemon.meta")).unwrap(), meta);
    }
}
