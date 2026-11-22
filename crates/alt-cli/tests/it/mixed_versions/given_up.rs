//! A merge that changes the index alone, killed before it writes the
//! index and given up: nothing is left of it that keeps the older program
//! from writing.

use super::step_mark::crashing;
use super::{new_bin, ok, old_bin, run, same_reads};

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_older_program_writes_after_a_step_with_nothing_to_put_back_was_given_up() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    // `main` changes the file `feature/side` removes: the merge stops with
    // the file as it is and the conflict in the index alone
    ok(run(&new, repo, &["init", "."]));
    let commit = |message: &str| {
        ok(run(&new, repo, &["add", "."]));
        ok(run(&new, repo, &["commit", "-m", message]));
    };
    std::fs::write(repo.join("f.txt"), "base\n").unwrap();
    std::fs::write(repo.join("k.txt"), "kept\n").unwrap();
    commit("base");
    ok(run(&new, repo, &["switch", "-c", "feature/side"]));
    std::fs::remove_file(repo.join("f.txt")).unwrap();
    commit("side removes f.txt");
    ok(run(&new, repo, &["switch", "main"]));
    std::fs::write(repo.join("f.txt"), "main\n").unwrap();
    commit("main");
    let status = || ok(run(&new, repo, &["status", "--json"]));
    let clean = status();

    let list = repo.join(".alt/points");
    let merge = ["merge", "feature/side"];
    let listed = crashing(repo, &merge, &format!("list:{}", list.display()));
    assert_eq!(listed.status.code(), Some(1));
    ok(run(&new, repo, &["merge", "--abort"]));
    let points = std::fs::read_to_string(&list).unwrap();
    assert!(!points.contains("checkout-file:before"), "{points}");
    let at = points.lines().position(|p| p == "checkout-index:before");
    let killed = crashing(repo, &merge, &(at.unwrap() + 1).to_string());
    assert_eq!(killed.status.code(), None);

    let pending = ok(run(&new, repo, &["operation", "pending"]));
    let id = pending.split(' ').next().unwrap();
    assert!(!run(&old, repo, &["branch", "kept"]).status.success());
    ok(run(&new, repo, &["operation", "abandon", id]));
    assert_eq!(status(), clean);
    assert!(!repo.join(".alt/MERGE_HEAD").exists());
    for bin in [&old, &new] {
        assert_eq!(ok(run(bin, repo, &["operation", "pending"])), "");
    }
    ok(run(&old, repo, &["branch", "kept"]));
    std::fs::write(repo.join("k.txt"), "by the older program\n").unwrap();
    ok(run(&old, repo, &["add", "k.txt"]));
    ok(run(&old, repo, &["commit", "-m", "k"]));
    same_reads(&old, repo);
}
