//! A step that was cut short, given up: where it had nothing to put back,
//! and by the abort of its operation and of no other.

use crate::step_crash::bench::{Bench, alt, copy, restore, run, said};

/// The command killed the first time it passes `point`, in the workspace
/// kept as `fixture`. The points it passes whole, and the operation that
/// waits.
fn killed_at(bench: &Bench, command: &[&str], point: &str) -> (String, String) {
    let repo = bench.repo();
    let list = bench.kept("points");
    let _ = std::fs::remove_file(&list);
    restore(&repo, &bench.kept("fixture"));
    let listing = format!("list:{}", list.display());
    let whole = alt(&repo, command, Some(&listing));
    assert_eq!(whole.status.code(), Some(1), "{}", said(&whole));
    let points = std::fs::read_to_string(&list).unwrap();
    let at = points.lines().position(|p| p == point).unwrap();
    restore(&repo, &bench.kept("fixture"));
    let killed = alt(&repo, command, Some(&(at + 1).to_string()));
    assert_eq!(killed.status.code(), None, "{}", said(&killed));
    let pending = run(&repo, &["operation", "pending"], 0);
    assert!(!pending.is_empty(), "nothing waits after {point}");
    (points, pending.split(' ').next().unwrap().to_owned())
}

#[test]
fn a_merge_that_changes_the_index_alone_is_given_up_before_the_index_is_written() {
    // `main` changes the file `side` removes: the merge stops with the
    // file as it is, and the conflict in the index alone
    let bench = Bench::new();
    let repo = bench.repo();
    run(&repo, &["init", "."], 0);
    bench.write("f.txt", "1\n");
    bench.write("k.txt", "k\n");
    bench.commit("files");
    run(&repo, &["switch", "-c", "side"], 0);
    std::fs::remove_file(repo.join("f.txt")).unwrap();
    bench.commit("side removes f.txt");
    run(&repo, &["switch", "main"], 0);
    bench.write("f.txt", "main\n");
    bench.commit("main");
    copy(&repo, &bench.kept("fixture"));
    let before = bench.shape();

    let merge = ["merge", "side"];
    let (points, id) = killed_at(&bench, &merge, "checkout-index:before");
    assert!(!points.contains("checkout-file:before"), "{points}");
    assert!(repo.join(".alt/MERGE_HEAD").exists());

    // all is as before the merge, and no record of it waits any more
    let given_up = run(&repo, &["operation", "abandon", &id], 0);
    assert!(given_up.contains("given up"), "{given_up}");
    assert_eq!(bench.shape(), before);
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");
    assert!(alt(&repo, &["fsck"], None).status.success());
    // nothing of the merge is left for an undo: it takes back the commit
    // that came before the merge
    run(&repo, &["undo"], 0);
    assert!(bench.shape().head.2.contains("files"));
}

#[test]
fn the_abort_of_another_operation_does_not_give_up_a_merge_that_was_cut_short() {
    let bench = Bench::new();
    let repo = bench.repo();
    run(&repo, &["init", "."], 0);
    bench.write("f.txt", "1\n2\n3\n");
    bench.commit("files");
    run(&repo, &["switch", "-c", "side"], 0);
    bench.write("f.txt", "1\nside\n3\n");
    bench.write("new.txt", "new\n");
    bench.commit("side");
    run(&repo, &["switch", "main"], 0);
    bench.write("f.txt", "1\nmain\n3\n");
    bench.commit("main");
    copy(&repo, &bench.kept("fixture"));

    // no file is written yet, and a directory that holds a file is where
    // the merge has one to write: it cannot be brought to an end
    killed_at(&bench, &["merge", "side"], "checkout-file:before");
    bench.write("new.txt/keep", "mine\n");
    let found = bench.shape();
    for other in ["cherry-pick", "revert", "rebase"] {
        let refused = alt(&repo, &[other, "--abort"], None);
        let told = said(&refused);
        assert!(!refused.status.success(), "{other}: {told}");
        assert!(told.contains("cannot be brought to an end"), "{told}");
        let whose = format!(
            "It is a command of a merge, which is not given up for the abort of a {other}: \
             `alt merge --abort` gives it up"
        );
        assert!(told.contains(&whose), "{other}: {told}");
        assert_eq!(bench.shape(), found, "{other}");
        assert_ne!(run(&repo, &["operation", "pending"], 0), "", "{other}");
    }

    // its own abort gives it up
    let aborted = run(&repo, &["merge", "--abort"], 0);
    assert!(aborted.contains("Merge aborted."), "{aborted}");
    assert!(!repo.join(".alt/MERGE_HEAD").exists());
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");
    assert_eq!(std::fs::read(repo.join("new.txt/keep")).unwrap(), b"mine\n");
    assert_eq!(std::fs::read(repo.join("f.txt")).unwrap(), b"1\nmain\n3\n");
}
