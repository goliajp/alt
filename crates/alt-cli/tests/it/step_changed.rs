//! A merge cut short as it writes where it stops, and a file it writes
//! changed before the next command: whatever was changed, some command
//! leads out, and what the user wrote is still there after it.
//!
//! A file the merge leaves in conflict is the user's to work on: the merge
//! is brought to an end around it. Any other is in the way and named, with
//! the two ways on: out of the way it is written as the merge has it, and
//! the merge given up with `--keep-changes` leaves it as it is.

use std::path::Path;

use crate::step_crash::bench::{Bench, alt, copy, restore, run, said};

/// `main` and `side` part after one commit: both change `f.txt`; `side`
/// alone changes `t.txt` and adds `new.txt`.
fn diverged() -> Bench {
    let bench = Bench::new();
    run(&bench.repo(), &["init", "."], 0);
    bench.write("f.txt", "1\n2\n3\n");
    bench.write("t.txt", "t\n");
    bench.commit("files");
    run(&bench.repo(), &["switch", "-c", "side"], 0);
    bench.write("f.txt", "1\nside\n3\n");
    bench.write("t.txt", "t by side\n");
    bench.write("new.txt", "new\n");
    bench.commit("side");
    run(&bench.repo(), &["switch", "main"], 0);
    bench.write("f.txt", "1\nmain\n3\n");
    bench.commit("main");
    copy(&bench.repo(), &bench.kept("fixture"));
    bench
}

/// The merge killed the first time it passes `point`, kept as `stuck`.
/// The operation that waits.
fn merge_killed_at(bench: &Bench, point: &str) -> String {
    let repo = bench.repo();
    let list = bench.kept("points");
    restore(&repo, &bench.kept("fixture"));
    let listing = format!("list:{}", list.display());
    let whole = alt(&repo, &["merge", "side"], Some(&listing));
    assert_eq!(whole.status.code(), Some(1), "{}", said(&whole));
    let points = std::fs::read_to_string(&list).unwrap();
    let at = points.lines().position(|p| p == point).unwrap();
    restore(&repo, &bench.kept("fixture"));
    let killed = alt(&repo, &["merge", "side"], Some(&(at + 1).to_string()));
    assert_eq!(killed.status.code(), None, "{}", said(&killed));
    let pending = run(&repo, &["operation", "pending"], 0);
    assert!(!pending.is_empty(), "nothing waits after {point}");
    pending.split(' ').next().unwrap().to_owned()
}

fn text(repo: &Path, file: &str) -> String {
    std::fs::read_to_string(repo.join(file)).unwrap()
}

/// The next command that writes brings the merge to an end.
fn brought_to_an_end(repo: &Path) -> String {
    let next = alt(repo, &["branch", "-d", "no-such-branch"], None);
    assert!(said(&next).contains("not found"), "{}", said(&next));
    assert_eq!(run(repo, &["operation", "pending"], 0), "");
    let status = run(repo, &["status", "--json"], 0);
    assert!(
        status.contains(r#""operation":{"kind":"merge"}"#),
        "{status}"
    );
    assert!(status.contains(r#""unmerged":["f.txt"]"#), "{status}");
    assert!(repo.join(".alt/MERGE_HEAD").exists());
    assert!(alt(repo, &["fsck"], None).status.success());
    status
}

/// Every command that writes, the abort and the plain giving up among
/// them, is refused over `file` and tells of both ways on.
fn in_the_way(bench: &Bench, file: &str, id: &str) {
    let repo = bench.repo();
    let found = bench.shape();
    let named = format!("'{file}' was changed after the operation was interrupted");
    for tried in [
        &["add", "f.txt"][..],
        &["commit", "-m", "m"],
        &["undo"],
        &["merge", "--abort"],
        &["operation", "recover", id],
        &["operation", "abandon", id],
    ] {
        let refused = alt(&repo, tried, None);
        let told = said(&refused);
        assert!(!refused.status.success(), "{tried:?}: {told}");
        assert!(told.contains(&named), "{tried:?}: {told}");
        assert!(told.contains("--keep-changes"), "{tried:?}: {told}");
        assert_eq!(bench.shape(), found, "{tried:?}");
    }
    let refused = said(&alt(&repo, &["add", "f.txt"], None));
    assert!(refused.contains("in its way aside"), "{refused}");
}

/// Given up with `--keep-changes`: all is as before the merge but `file`,
/// which holds what the user wrote.
fn given_up_around(bench: &Bench, file: &str, id: &str) {
    let repo = bench.repo();
    let given_up = alt(&repo, &["operation", "abandon", id, "--keep-changes"], None);
    assert!(given_up.status.success(), "{}", said(&given_up));
    let left = format!("left as it is: {file}\n");
    assert!(said(&given_up).ends_with(&left), "{}", said(&given_up));
    assert_eq!(text(&repo, file), "mine\n");
    assert_eq!(text(&repo, "f.txt"), "1\nmain\n3\n");
    assert!(!repo.join(".alt/MERGE_HEAD").exists());
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");
    let status = run(&repo, &["status", "--json"], 0);
    assert!(status.contains(r#""operation":null"#), "{status}");
    assert!(alt(&repo, &["fsck"], None).status.success());
    // the merge is begun anew once the file is out of its way
    std::fs::rename(repo.join(file), repo.join("mine")).unwrap();
    if file == "t.txt" {
        bench.write("t.txt", "t\n");
    }
    run(&repo, &["merge", "side"], 1);
    assert_eq!(text(&repo, "t.txt"), "t by side\n");
    assert_eq!(text(&repo, "new.txt"), "new\n");
    assert_eq!(text(&repo, "mine"), "mine\n");
}

#[test]
fn a_conflict_file_changed_before_the_merge_is_brought_to_an_end_keeps_the_change() {
    let bench = diverged();
    let repo = bench.repo();
    // every file written and the index not; or no file written yet
    for point in ["checkout-index:before", "checkout-file:before"] {
        merge_killed_at(&bench, point);
        bench.write("f.txt", "mine\n");
        brought_to_an_end(&repo);
        assert_eq!(text(&repo, "f.txt"), "mine\n", "{point}");
        assert_eq!(text(&repo, "t.txt"), "t by side\n", "{point}");
        assert_eq!(text(&repo, "new.txt"), "new\n", "{point}");
    }
    // the merge goes on from there as any other
    run(&repo, &["add", "f.txt"], 0);
    run(&repo, &["commit", "-m", "merged"], 0);
    let merged = run(&repo, &["cat-file", "-p", "HEAD"], 0);
    assert_eq!(merged.matches("\nparent ").count(), 2, "{merged}");

    // and its abort writes over the conflict, changed or not, as git does
    merge_killed_at(&bench, "checkout-index:before");
    bench.write("f.txt", "mine\n");
    let aborted = run(&repo, &["merge", "--abort"], 0);
    assert!(aborted.contains("Merge aborted."), "{aborted}");
    assert_eq!(text(&repo, "f.txt"), "1\nmain\n3\n");
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");
}

#[test]
fn a_merged_file_changed_before_the_merge_is_brought_to_an_end_is_named_and_kept() {
    let bench = diverged();
    let repo = bench.repo();
    let id = merge_killed_at(&bench, "checkout-index:before");
    bench.write("t.txt", "mine\n");
    copy(&repo, &bench.kept("stuck"));
    in_the_way(&bench, "t.txt", &id);
    given_up_around(&bench, "t.txt", &id);

    // out of the way, the merge is brought to an end
    restore(&repo, &bench.kept("stuck"));
    std::fs::rename(repo.join("t.txt"), repo.join("mine")).unwrap();
    brought_to_an_end(&repo);
    assert_eq!(text(&repo, "t.txt"), "t by side\n");
    assert_eq!(text(&repo, "mine"), "mine\n");
}

#[test]
fn a_file_made_where_the_merge_has_one_to_write_is_named_and_kept() {
    let bench = diverged();
    let repo = bench.repo();
    let id = merge_killed_at(&bench, "checkout-file:before");
    assert!(!repo.join("new.txt").exists());
    bench.write("new.txt", "mine\n");
    copy(&repo, &bench.kept("stuck"));
    in_the_way(&bench, "new.txt", &id);
    given_up_around(&bench, "new.txt", &id);

    restore(&repo, &bench.kept("stuck"));
    std::fs::rename(repo.join("new.txt"), repo.join("mine")).unwrap();
    brought_to_an_end(&repo);
    assert_eq!(text(&repo, "new.txt"), "new\n");
    assert_eq!(text(&repo, "mine"), "mine\n");
}
