//! This build's first command in a repository an older build wrote is an
//! `alt add`, which syncs nothing: the log's marker, which the older
//! build left without a boundary in this format, records what the log
//! held as synced before the add marks it. The history the older build
//! wrote is not looked at again, also where a ref transaction of it
//! names an object that was since taken out of the store on request.

use std::fs;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefStore, RefTarget};

use super::super::super::log_tails::oplog_flags;
use super::super::super::mixed_versions::{new_bin, old_bin, run, same_reads};
use super::super::super::staged_durability::{add, repo};
use super::super::super::sync_order::{FILES, log_len, log_synced, objects_synced};
use super::super::another_boot;
use super::{ok, text};

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn an_add_first_after_an_older_build_records_the_history_it_wrote_as_synced() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    ok(run(&old, dir, &["init"]));
    for n in 0..3 {
        fs::write(dir.join(format!("f{n}.txt")), format!("f{n}\n")).unwrap();
        ok(run(&old, dir, &["add", "."]));
        ok(run(&old, dir, &["commit", "-m", &format!("commit {n}")]));
    }
    let alt_dir = dir.join(".alt");
    let held = log_len(&alt_dir);
    add(dir, "b.txt");
    let marked = (oplog_flags(&alt_dir) & 1, log_synced(&alt_dir));
    assert_eq!(marked, (1, held));
    for bin in [&old, &new] {
        assert_eq!(ok(run(bin, dir, &["log"])).lines().count(), 3);
    }
    let committed = run(&new, dir, &["commit", "-m", "fourth"]);
    let said = text(&committed.stderr);
    assert!(committed.status.success(), "{said}");
    assert!(!said.contains("discarded"), "{said}");
    assert_eq!(ok(run(&new, dir, &["log"])).lines().count(), 4);
    same_reads(&old, dir);
}

/// A tag at the blob `b.txt` is staged as, synced; the blob lost to a
/// power cut, an older build first after it, and the record that names
/// the blob taken out with `--committed`: the tag's transaction lies
/// before the log's boundary and names an object that is not stored.
#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn history_that_names_an_object_taken_out_on_request_is_kept_by_an_add_and_a_commit() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    add(dir, "b.txt");
    let alt_dir = dir.join(".alt");
    let blob = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"b.txt\n");
    let tag = RefChange {
        name: "refs/tags/kept".to_owned(),
        old: None,
        new: Some(RefTarget::Oid(blob)),
    };
    let mut refs = RefStore::open(&alt_dir).unwrap();
    refs.commit("test", 0, &[tag]).unwrap();
    drop(refs);
    for (name, len) in FILES.iter().zip(objects_synced(&alt_dir)).take(3) {
        let file = fs::OpenOptions::new().write(true).open(alt_dir.join(name));
        file.unwrap().set_len(len).unwrap();
    }
    another_boot(&alt_dir);
    fs::write(dir.join("c.txt"), "c\n").unwrap();
    ok(run(&old, dir, &["add", "c.txt"]));
    let taken = ok(run(
        &new,
        dir,
        &["fsck", "--drop-lost", "--yes", "--committed"],
    ));
    assert!(taken.contains("took 1 record(s) out"), "{taken}");

    // the file is staged anew with other content: the blob the tag
    // names stays out of the store
    let kept = format!("{blob}\n");
    let goes_on = |what: &str| {
        fs::write(dir.join("b.txt"), format!("{what}\n")).unwrap();
        for args in [&["add", "b.txt"][..], &["commit", "-m", what]] {
            let done = run(&new, dir, args);
            let said = text(&done.stderr);
            assert!(done.status.success(), "{what} {args:?}: {said}");
            assert!(!said.contains("discarded"), "{what} {args:?}: {said}");
        }
        assert_eq!(ok(run(&new, dir, &["rev-parse", "kept"])), kept, "{what}");
    };
    goes_on("after an add that synced nothing");
    assert_eq!(ok(run(&new, dir, &["log"])).lines().count(), 2);
    // and where the marker is gone when a command that syncs comes first
    fs::remove_file(alt_dir.join("oplog/durable")).unwrap();
    let tagged = run(&new, dir, &["tag", "over-no-marker"]);
    assert!(tagged.status.success(), "{}", text(&tagged.stderr));
    goes_on("after a tag over no marker");
    assert_eq!(ok(run(&new, dir, &["log"])).lines().count(), 3);
    assert!(!ok(run(&new, dir, &["cat-file", "-t", "HEAD"])).is_empty());
    let read = run(&new, dir, &["cat-file", "-p", &blob.to_string()]);
    assert!(!read.status.success(), "the blob is stored again");
}
