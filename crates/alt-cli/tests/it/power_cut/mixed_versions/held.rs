//! A removal of lost records is a command that writes: it holds the lock
//! of its workspace and the write lock of the object store, runs once
//! under a request id, and goes through where the other commands that
//! write are refused: beside the lost records, with an operation of the
//! workspace left open.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use alt_os::sys::{LOCK_EX, LOCK_UN, flock};

use super::super::super::mixed_versions::{new_bin, old_bin, run, same_reads};
use super::super::super::staged_durability::repo;
use super::super::super::sync_order::lens;
use super::super::base::copy_tree;
use super::{an_older_build_wrote_past_a_lost_blob, ok, text};

/// A library that ends the process, as a kill would, when it comes to
/// the sync that `ALT_TEST_DIE_AT` counts to.
pub(super) fn dying(root: &Path) -> PathBuf {
    let source = root.join("dies-at-a-sync.c");
    let library = root.join("dies-at-a-sync.so");
    fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <unistd.h>
static int syncs;
static void count(void) {
    const char *at = getenv("ALT_TEST_DIE_AT");
    if (at && ++syncs == atoi(at)) _exit(99);
}
int fsync(int fd) { count(); int (*real)(int) = dlsym(RTLD_NEXT, "fsync"); return real(fd); }
int fdatasync(int fd) { count(); int (*real)(int) = dlsym(RTLD_NEXT, "fdatasync"); return real(fd); }
"#,
    )
    .unwrap();
    let built = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2", "-o"])
        .arg(&library)
        .arg(&source)
        .arg("-ldl")
        .output()
        .unwrap();
    assert!(built.status.success(), "{}", text(&built.stderr));
    library
}

/// Runs `bin` in `dir` and ends it at its sync number `at`. `None` when
/// it made fewer syncs than that and answered.
fn cut_short(library: &Path, bin: &Path, dir: &Path, args: &[&str], at: usize) -> Option<()> {
    let out = Command::new(bin)
        .current_dir(dir)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("LD_PRELOAD", library)
        .env("ALT_TEST_DIE_AT", at.to_string())
        .args(args)
        .output()
        .unwrap();
    match out.status.code() {
        Some(99) => Some(()),
        _ => {
            assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
            None
        }
    }
}

pub(super) const TAKE_OUT: [&str; 3] = ["fsck", "--drop-lost", "--yes"];

/// Whether the process `pid` waits for a lock of the file `inode`.
fn waits_for(pid: u32, inode: u64) -> bool {
    let locks = fs::read_to_string("/proc/locks").unwrap();
    locks.lines().any(|line| {
        let mut fields = line.split_whitespace().skip(1);
        fields.next() == Some("->")
            && fields.nth(3) == Some(&pid.to_string())
            && fields.next().and_then(|file| file.rsplit(':').next()) == Some(&inode.to_string())
    })
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_removal_waits_for_the_lock_of_its_workspace_and_for_that_of_the_object_store() {
    let (old, new) = (old_bin(), new_bin());
    for lock in ["write.lock", "odb.lock"] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        repo(dir);
        an_older_build_wrote_past_a_lost_blob(&old, dir);
        let alt_dir = dir.join(".alt");
        let before = lens(&alt_dir);
        let held = fs::File::open(alt_dir.join(lock)).unwrap();
        flock(&held, LOCK_EX).unwrap();
        // listing changes nothing and waits for no lock of the workspace
        if lock == "write.lock" {
            let listed = run(&new, dir, &["fsck", "--drop-lost"]);
            assert_eq!(listed.status.code(), Some(1), "{}", text(&listed.stderr));
        }
        let mut removal = Command::new(&new)
            .current_dir(dir)
            .env("ALT_NO_DAEMON", "1")
            .args(TAKE_OUT)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let inode = held.metadata().unwrap().ino();
        while !waits_for(removal.id(), inode) {
            let ended = removal.try_wait().unwrap();
            assert!(ended.is_none(), "{lock}: it went on without the lock");
            std::thread::yield_now();
        }
        assert_eq!(lens(&alt_dir), before, "{lock}");
        flock(&held, LOCK_UN).unwrap();
        let taken = ok(removal.wait_with_output().unwrap());
        assert!(taken.contains("took 1 record(s) out"), "{lock}: {taken}");
        assert_eq!(lens(&alt_dir)[3], before[3] - 82, "{lock}");
    }
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_removal_asked_for_again_under_its_request_id_answers_as_before_and_runs_once() {
    const ID: &str = "0123456789abcdef0123456789abcdef";
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    an_older_build_wrote_past_a_lost_blob(&old, dir);
    let args = ["--request-id", ID, "fsck", "--drop-lost", "--yes"];
    let taken = ok(run(&new, dir, &args));
    assert!(taken.contains("took 1 record(s) out"), "{taken}");
    let after = lens(&dir.join(".alt"));
    assert_eq!(ok(run(&new, dir, &args)), taken);
    assert_eq!(lens(&dir.join(".alt")), after);
    // listing is no write and takes no request id
    let listed = run(&new, dir, &["--request-id", ID, "fsck", "--drop-lost"]);
    let said = text(&listed.stderr);
    assert!(!listed.status.success(), "{said}");
    assert!(
        said.contains("request identities require a write"),
        "{said}"
    );
}

fn pending(new: &Path, dir: &Path) -> Vec<String> {
    let listed = ok(run(new, dir, &["operation", "pending", "--json"]));
    let listed: alt_json::Value = alt_json::from_str(&listed).unwrap();
    let operations = listed["operations"].as_array().unwrap().iter();
    operations
        .map(|op| op["operation_id"].as_str().unwrap().to_owned())
        .collect()
}

/// The repository in `base` copied to a directory beside it for each
/// sync of `args` run by `bin`, with the command ended at that sync.
pub(super) fn each_cut_short<'a>(
    library: &'a Path,
    base: &'a Path,
    bin: &'a Path,
    args: &'a [&'a str],
) -> impl Iterator<Item = (usize, PathBuf)> + 'a {
    (1..).map_while(move |at| {
        let dir = base.with_file_name(format!("cut-{at}"));
        copy_tree(base, &dir);
        cut_short(library, bin, &dir, args, at).map(|()| (at, dir))
    })
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_removal_goes_through_where_an_add_of_the_workspace_was_left_open() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let base = tmp.path().join("base");
    fs::create_dir(&base).unwrap();
    repo(&base);
    an_older_build_wrote_past_a_lost_blob(&old, &base);
    fs::write(base.join("d.txt"), "d\n").unwrap();
    let library = dying(tmp.path());
    // the older build is killed between the intent of its add and the
    // record that the add is complete
    // the last such sync: the intent is synced by then
    let cuts = each_cut_short(&library, &base, &old, &["add", "d.txt"]);
    let (dir, left) = cuts
        .filter_map(|(_, dir)| match &pending(&new, &dir)[..] {
            [left] => Some((dir.clone(), left.clone())),
            _ => None,
        })
        .last()
        .expect("a sync the add is left open at");
    let dir = &dir;
    fs::write(dir.join("e.txt"), "e\n").unwrap();
    let refused = run(&new, dir, &["add", "e.txt"]);
    assert!(!refused.status.success(), "{}", text(&refused.stdout));
    let taken = ok(run(&new, dir, &TAKE_OUT));
    assert!(taken.contains("took 1 record(s) out"), "{taken}");
    // the add is as open as it was
    assert_eq!(pending(&new, dir), std::slice::from_ref(&left));
    ok(run(&new, dir, &["operation", "recover", &left]));
    assert!(pending(&new, dir).is_empty());
    ok(run(&new, dir, &["add", "b.txt", "d.txt", "e.txt"]));
    ok(run(&new, dir, &["commit", "-m", "second"]));
    same_reads(&old, dir);
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn an_add_left_open_is_abandoned_by_the_next_add_once_a_blob_of_its_index_was_taken_out() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let base = tmp.path().join("base");
    fs::create_dir(&base).unwrap();
    repo(&base);
    an_older_build_wrote_past_a_lost_blob(&old, &base);
    fs::write(base.join("d.txt"), "d\n").unwrap();
    let library = dying(tmp.path());
    let cuts = each_cut_short(&library, &base, &old, &["add", "d.txt"]);
    let (dir, left) = cuts
        .filter_map(|(_, dir)| match &pending(&new, &dir)[..] {
            [left] => Some((dir.clone(), left.clone())),
            _ => None,
        })
        .last()
        .expect("a sync the add is left open at");
    let dir = &dir;
    fs::write(dir.join("e.txt"), "e\n").unwrap();
    // beside the lost record the open add is in the way of every add
    let refused = run(&new, dir, &["add", "e.txt"]);
    assert!(!refused.status.success(), "{}", text(&refused.stdout));
    let said = text(&refused.stderr);
    let open = format!("operation {left} has no durable completion; recovery required");
    assert!(said.contains(&open), "{said}");
    ok(run(&new, dir, &TAKE_OUT));
    assert_eq!(pending(&new, dir), std::slice::from_ref(&left));
    // the index the add prepared names b.txt, whose blob is now known
    // not to be stored: the add cannot be completed, and the next
    // command that writes abandons it and goes on
    let added = run(&new, dir, &["add", "e.txt"]);
    let said = text(&added.stderr);
    assert!(added.status.success(), "{said}");
    assert_eq!(text(&added.stdout), "staged 1 path(s)\n");
    let abandoned = format!("warning: abandoned operation {left}: it was interrupted");
    assert!(said.contains(&abandoned), "{said}");
    let why = "index entry b.txt of the index it prepared names blob ";
    assert!(
        said.contains(why) && said.contains("add the file again"),
        "{said}"
    );
    assert!(pending(&new, dir).is_empty());
    let told = ok(run(&new, dir, &["operation", "status", &left]));
    assert!(told.contains("abandoned"), "{told}");
    // the index is the one from before that add, with what was added now
    let status = ok(run(&new, dir, &["status"]));
    let (staged, untracked) = status.split_once("Untracked files:").unwrap();
    assert!(
        untracked.contains("d.txt") && !staged.contains("d.txt"),
        "{status}"
    );
    for name in ["b.txt", "c.txt", "e.txt"] {
        assert!(staged.contains(name), "{status}");
    }
    ok(run(&new, dir, &["add", "b.txt", "d.txt"]));
    ok(run(&new, dir, &["commit", "-m", "second"]));
    same_reads(&old, dir);
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
}
