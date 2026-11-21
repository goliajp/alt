//! A removal of lost records ended at each of its syncs, in a store
//! whose marker is an older build's: whoever comes next finishes it or
//! makes it again, and both builds go on.

use std::fs;
use std::path::Path;

use super::super::super::mixed_versions::{new_bin, old_bin, run, same_reads};
use super::super::super::staged_durability::repo;
use super::super::base::copy_tree;
use super::held::{TAKE_OUT, dying, each_cut_short};
use super::{an_older_build_wrote_past_a_lost_blob, text};

/// What `bin` answers to `args` in `dir`, which it has to succeed at.
fn went(bin: &Path, dir: &Path, args: &[&str], case: &str) -> String {
    let out = run(bin, dir, args);
    let said = text(&out.stderr);
    assert!(out.status.success(), "{case}: {args:?}: {said}");
    String::from_utf8(out.stdout).unwrap()
}

/// Both builds go on from `dir`, whose lost records are out: each adds
/// and commits, and the check passes.
fn both_go_on(old: &Path, new: &Path, dir: &Path, case: &str) {
    let none = went(new, dir, &["fsck", "--drop-lost"], case);
    assert_eq!(
        none, "no record names content that is not stored\n",
        "{case}"
    );
    assert!(!dir.join(".alt/odb.repair").exists(), "{case}");
    for bin in [old, new] {
        let status = went(bin, dir, &["status"], case);
        assert!(status.contains("new file:   c.txt"), "{case}: {status}");
    }
    // the older build writes first, by the marker the removal left
    fs::write(dir.join("f.txt"), "f\n").unwrap();
    went(old, dir, &["add", "f.txt"], case);
    fs::write(dir.join("d.txt"), "d\n").unwrap();
    went(new, dir, &["add", "b.txt", "d.txt"], case);
    went(old, dir, &["commit", "-m", "second"], case);
    same_reads(old, dir);
    fs::write(dir.join("e.txt"), "e\n").unwrap();
    went(new, dir, &["add", "e.txt"], case);
    went(new, dir, &["commit", "-m", "third"], case);
    same_reads(old, dir);
    let checked = went(new, dir, &["fsck"], case);
    assert!(
        checked.ends_with("every record verified\n"),
        "{case}: {checked}"
    );
    for name in ["b.txt", "c.txt", "d.txt", "e.txt", "f.txt"] {
        let shown = went(new, dir, &["show", &format!("HEAD:{name}")], case);
        assert_eq!(
            shown.as_bytes(),
            fs::read(dir.join(name)).unwrap(),
            "{case}"
        );
    }
}

/// The marker of the object store is the older build's, which wrote
/// last: it records neither a generation nor how long the files were.
#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_removal_cut_short_at_any_sync_is_finished_and_both_builds_go_on() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let base = tmp.path().join("base");
    fs::create_dir(&base).unwrap();
    repo(&base);
    an_older_build_wrote_past_a_lost_blob(&old, &base);
    let library = dying(tmp.path());
    let marker = |dir: &Path| fs::read(dir.join(".alt/odb.durable")).unwrap();
    let map_len = |dir: &Path| fs::metadata(dir.join(".alt/map.alt")).unwrap().len();
    assert_eq!(marker(&base).len(), 24, "the marker is the older build's");
    let (mut cuts, mut kept, mut longer) = (0, 0, 0);
    for (at, dir) in each_cut_short(&library, &base, &new, &TAKE_OUT) {
        cuts += 1;
        kept += usize::from(dir.join(".alt/odb.repair").exists());
        // the removal is asked for again; or it is only looked for, which
        // finishes one that was kept; or the older build goes first
        let again = dir.with_file_name(format!("listed-{at}"));
        let older = dir.with_file_name(format!("older-{at}"));
        copy_tree(&dir, &again);
        copy_tree(&dir, &older);
        let case = format!("sync {at}, asked for again");
        went(&new, &dir, &TAKE_OUT, &case);
        both_go_on(&old, &new, &dir, &case);
        let case = format!("sync {at}, looked for");
        let listed = run(&new, &again, &["fsck", "--drop-lost"]);
        if listed.status.code() == Some(1) {
            went(&new, &again, &TAKE_OUT, &case);
        } else {
            assert!(listed.status.success(), "{case}: {}", text(&listed.stderr));
        }
        // the older build's marker is left as it was, with the length
        // `map.alt` had before the record went
        let synced = u64::from_le_bytes(marker(&again)[16..24].try_into().unwrap());
        longer += usize::from(marker(&again).len() == 24 && synced > map_len(&again));
        both_go_on(&old, &new, &again, &case);
        let case = format!("sync {at}, older first");
        went(&old, &older, &["status"], &case);
        fs::write(older.join("g.txt"), "g\n").unwrap();
        went(&old, &older, &["add", "g.txt"], &case);
        went(&new, &older, &TAKE_OUT, &case);
        both_go_on(&old, &new, &older, &case);
        for dir in [dir, again, older] {
            fs::remove_dir_all(dir).unwrap();
        }
    }
    // the syncs of the request, of the files before anything is kept,
    // and those of the removal itself
    assert!(
        cuts >= 12 && kept >= 4 && longer >= kept,
        "{cuts} syncs, {kept} with a removal kept, {longer} with the older marker past the file"
    );
}
