//! `status`, `diff` and `add` work from the snapshot beside the index
//! (`index.scan`): a cache that may be missing, damaged or older than the
//! index, and that must never hide a change made to a file.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn repo() -> alt_os::temp::TempDir {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init"]));
    write(root, "a.txt", "aaaa\n");
    write(root, "src/lib.rs", "lib\n");
    write(root, "src/deep/mod.rs", "mod\n");
    write(root, "docs/guide.md", "guide\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    dir
}

fn status(root: &Path) -> String {
    ok(alt(root, &["status"]))
}

const CLEAN: &str = "On branch main\nnothing to commit, working tree clean\n";

#[test]
fn a_rewrite_of_the_same_size_right_after_a_status_is_seen() {
    let dir = repo();
    let root = dir.path();
    assert_eq!(status(root), CLEAN);
    // the rewrite lands in the tick of the clock the status ran in, or the
    // one the file was last written in: its size and mtime may not move
    for round in 0..40 {
        write(root, "a.txt", "bbbb\n");
        assert!(status(root).contains("modified:   a.txt"), "round {round}");
        assert!(ok(alt(root, &["diff"])).contains("+bbbb"), "round {round}");
        write(root, "a.txt", "aaaa\n");
        assert_eq!(status(root), CLEAN, "round {round}");
    }
}

#[test]
fn a_staged_file_rewritten_at_once_is_seen() {
    let dir = repo();
    let root = dir.path();
    for round in 0..20 {
        write(root, "a.txt", &format!("st{round:02}\n"));
        ok(alt(root, &["add", "a.txt"]));
        write(root, "a.txt", &format!("wt{round:02}\n"));
        let st = status(root);
        assert!(
            st.contains("Changes to be committed:\n\tmodified:   a.txt"),
            "round {round}: {st}"
        );
        assert!(
            st.contains("Changes not staged for commit:\n\tmodified:   a.txt"),
            "round {round}: {st}"
        );
    }
}

#[test]
fn the_snapshot_settles_and_is_then_left_alone() {
    let dir = repo();
    let root = dir.path();
    let scan = root.join(".alt/index.scan");
    // a file written within the current tick is not recorded at once
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(status(root), CLEAN);
    let settled = std::fs::read(&scan).unwrap();
    assert_eq!(status(root), CLEAN);
    assert_eq!(ok(alt(root, &["diff"])), "");
    assert_eq!(std::fs::read(&scan).unwrap(), settled);
    // it names the index it was built from by the index's checksum
    let index = std::fs::read(root.join(".alt/index")).unwrap();
    assert_eq!(&settled[16..36], &index[index.len() - 20..]);
}

#[test]
fn a_missing_or_damaged_snapshot_changes_no_answer() {
    let dir = repo();
    let root = dir.path();
    write(root, "a.txt", "changed\n");
    write(root, "new.txt", "new\n");
    std::fs::remove_file(root.join("docs/guide.md")).unwrap();
    write(root, "src/lib.rs", "staged\n");
    ok(alt(root, &["add", "src/lib.rs"]));
    let want = (
        status(root),
        ok(alt(root, &["diff"])),
        ok(alt(root, &["diff", "--cached"])),
    );
    let scan = root.join(".alt/index.scan");
    let whole = std::fs::read(&scan).unwrap();
    let damaged: Vec<Vec<u8>> = vec![
        Vec::new(),
        whole[..whole.len() / 2].to_vec(),
        whole
            .iter()
            .enumerate()
            .map(|(i, b)| if i == 100 { b ^ 0x40 } else { *b })
            .collect(),
        vec![0; whole.len()],
        b"ALTWTC99 a later version".to_vec(),
    ];
    for bytes in damaged {
        std::fs::write(&scan, &bytes).unwrap();
        let got = (
            status(root),
            ok(alt(root, &["diff"])),
            ok(alt(root, &["diff", "--cached"])),
        );
        assert_eq!(got, want);
    }
    std::fs::remove_file(&scan).unwrap();
    assert_eq!(status(root), want.0);
    std::fs::remove_file(&scan).unwrap();
    assert_eq!(ok(alt(root, &["diff"])), want.1);
}

#[test]
fn an_index_written_by_a_program_that_knows_no_snapshot_is_followed() {
    let dir = repo();
    let root = dir.path();
    let scan = root.join(".alt/index.scan");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(status(root), CLEAN);
    let old_scan = std::fs::read(&scan).unwrap();
    let old_index = std::fs::read(root.join(".alt/index")).unwrap();

    // the index moves on while the snapshot stays behind, as when an
    // older alt stages a file
    write(root, "a.txt", "staged by another program\n");
    write(root, "added.txt", "added\n");
    ok(alt(root, &["add", "a.txt", "added.txt"]));
    let want = status(root);
    assert!(want.contains("modified:   a.txt") && want.contains("new file:   added.txt"));
    std::fs::write(&scan, &old_scan).unwrap();
    assert_eq!(status(root), want);
    assert_eq!(ok(alt(root, &["diff"])), "");

    // and the other way round: a snapshot of a later index beside an older one
    let new_scan = std::fs::read(&scan).unwrap();
    ok(alt(root, &["commit", "-m", "second"]));
    std::fs::write(root.join(".alt/index"), &old_index).unwrap();
    std::fs::write(&scan, &new_scan).unwrap();
    let st = status(root);
    assert!(
        st.contains("Changes to be committed:\n\tmodified:   a.txt\n\tdeleted:   added.txt"),
        "{st}"
    );
    assert!(
        st.contains("Changes not staged for commit:\n\tmodified:   a.txt"),
        "{st}"
    );
    assert!(st.contains("Untracked files:\n\tadded.txt"), "{st}");
}

#[test]
fn a_switch_leaves_a_snapshot_that_still_sees_every_change() {
    let dir = repo();
    let root = dir.path();
    ok(alt(root, &["switch", "-c", "other"]));
    write(root, "a.txt", "other\n");
    write(root, "src/only-here.rs", "only\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "other"]));
    for round in 0..10 {
        let (to, content) = if round % 2 == 0 {
            ("main", "aaaa\n")
        } else {
            ("other", "other\n")
        };
        ok(alt(root, &["switch", to]));
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            content
        );
        let index = std::fs::read(root.join(".alt/index")).unwrap();
        let scan = std::fs::read(root.join(".alt/index.scan")).unwrap();
        assert_eq!(
            &scan[16..36],
            &index[index.len() - 20..],
            "the switch wrote the snapshot"
        );
        assert_eq!(
            status(root),
            format!("On branch {to}\nnothing to commit, working tree clean\n")
        );
        // same size as what the switch just wrote, and within the same tick
        let rewritten = content.replace(|c: char| c.is_ascii_lowercase(), "z");
        write(root, "a.txt", &rewritten);
        assert!(status(root).contains("modified:   a.txt"), "round {round}");
        write(root, "a.txt", content);
        assert_eq!(
            status(root),
            format!("On branch {to}\nnothing to commit, working tree clean\n")
        );
    }
}

#[test]
fn tracked_files_beside_a_nested_repository_are_not_reported_deleted() {
    let dir = repo();
    let root = dir.path();
    write(root, "src/.git/HEAD", "ref: refs/heads/main\n");
    write(
        root,
        "src/untracked.rs",
        "belongs to the nested repository\n",
    );
    assert_eq!(status(root), CLEAN);
    write(root, "src/lib.rs", "changed\n");
    assert_eq!(
        status(root),
        "On branch main\nChanges not staged for commit:\n\tmodified:   src/lib.rs\n"
    );
}

#[test]
fn each_workspace_keeps_its_own_snapshot() {
    let dir = repo();
    let root = dir.path();
    let side = alt_os::temp::tempdir().unwrap();
    let ws = side.path().join("ws");
    ok(alt(
        root,
        &["workspace", "add", "side", ws.to_str().unwrap()],
    ));
    write(&ws, "a.txt", "side\n");
    assert!(status(&ws).contains("modified:   a.txt"));
    assert_eq!(status(root), CLEAN);
    assert!(root.join(".alt/workspaces/side/index.scan").exists());
    ok(alt(&ws, &["add", "a.txt"]));
    assert!(status(&ws).contains("Changes to be committed:\n\tmodified:   a.txt"));
    assert_eq!(status(root), CLEAN);
}

#[test]
fn adding_everything_in_an_empty_tree_stages_nothing() {
    let dir = alt_os::temp::tempdir().unwrap();
    ok(alt(dir.path(), &["init"]));
    assert_eq!(ok(alt(dir.path(), &["add", "."])), "staged 0 path(s)\n");
    assert!(!alt(dir.path(), &["add", "missing"]).status.success());
}

#[test]
fn adding_everything_counts_every_file_and_touches_only_what_changed() {
    let dir = repo();
    let root = dir.path();
    write(root, "a.txt", "changed\n");
    write(root, "new/file.txt", "new\n");
    std::fs::remove_file(root.join("docs/guide.md")).unwrap();
    assert_eq!(ok(alt(root, &["add", "."])), "staged 4 path(s)\n");
    assert_eq!(
        status(root),
        "On branch main\nChanges to be committed:\n\tmodified:   a.txt\n\tdeleted:   docs/guide.md\n\tnew file:   new/file.txt\n"
    );
    // an unchanged file edited afterwards does not stand in the way of undo
    write(root, "src/lib.rs", "edited after the add\n");
    ok(alt(root, &["undo"]));
    let st = status(root);
    assert!(!st.contains("Changes to be committed"), "{st}");
    assert!(
        st.contains("modified:   a.txt") && st.contains("modified:   src/lib.rs"),
        "{st}"
    );
    assert_eq!(ok(alt(root, &["add", "."])), "staged 4 path(s)\n");
    assert_eq!(ok(alt(root, &["add", "."])), "staged 4 path(s)\n");
    assert_eq!(ok(alt(root, &["add", "src"])), "staged 2 path(s)\n");
}
