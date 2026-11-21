//! A merge or a cherry-pick one program stopped and the other gave up or
//! went on with.

use super::{new_bin, ok, old_bin, run, same_reads};

/// `main` and `feature/side` both rewrite `f.txt`.
fn diverged(repo: &std::path::Path) {
    let new = new_bin();
    let write = |text: &str, message: &str| {
        std::fs::write(repo.join("f.txt"), text).unwrap();
        ok(run(&new, repo, &["add", "f.txt"]));
        ok(run(&new, repo, &["commit", "-m", message]));
    };
    ok(run(&new, repo, &["init", "."]));
    write("base\n", "base");
    ok(run(&new, repo, &["switch", "-c", "feature/side"]));
    write("side\n", "side");
    ok(run(&new, repo, &["switch", "main"]));
    write("main\n", "main");
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_merge_either_program_stopped_is_given_up_by_the_new_one() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    diverged(repo);
    let status = || ok(run(&new, repo, &["status", "--json"]));
    let head = ok(run(&new, repo, &["rev-parse", "HEAD"]));

    for stops in [&old, &new] {
        let stopped = run(stops, repo, &["merge", "feature/side"]);
        assert_eq!(stopped.status.code(), Some(1));
        assert!(status().contains(r#""operation":{"kind":"merge"}"#));
        ok(run(&new, repo, &["merge", "--abort"]));
        assert!(status().contains(r#""clean":true"#), "{}", status());
        assert_eq!(ok(run(&new, repo, &["rev-parse", "HEAD"])), head);
        assert_eq!(std::fs::read(repo.join("f.txt")).unwrap(), b"main\n");
        same_reads(&old, repo);
    }

    // a merge the older program concluded is none to give up
    let stopped = run(&new, repo, &["merge", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    std::fs::write(repo.join("f.txt"), "both\n").unwrap();
    ok(run(&old, repo, &["add", "f.txt"]));
    ok(run(
        &old,
        repo,
        &["commit", "-m", "merged by the older program"],
    ));
    assert!(!run(&new, repo, &["merge", "--abort"]).status.success());
    same_reads(&old, repo);
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_pick_is_given_up_or_gone_on_with_only_where_the_new_program_stopped_it() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    diverged(repo);
    let status = || ok(run(&new, repo, &["status", "--json"]));
    let head = ok(run(&new, repo, &["rev-parse", "HEAD"]));

    // stopped by the new program and given up: the older one reads the same
    let stopped = run(&new, repo, &["cherry-pick", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    ok(run(&new, repo, &["cherry-pick", "--abort"]));
    assert!(status().contains(r#""clean":true"#), "{}", status());
    same_reads(&old, repo);

    // stopped by the older program, which leaves no file: nothing is done
    let stopped = run(&old, repo, &["cherry-pick", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    let before = status();
    for flag in ["--abort", "--continue"] {
        let refused = run(&new, repo, &["cherry-pick", flag]);
        assert!(!refused.status.success(), "{flag}");
        let said = String::from_utf8_lossy(&refused.stderr).into_owned();
        assert!(said.contains("no cherry-pick in progress"), "{said}");
    }
    assert_eq!(status(), before);
    assert!(before.contains(r#""unmerged":["f.txt"]"#), "{before}");

    // concluded by the older program's commit, then picked anew and gone on with
    std::fs::write(repo.join("f.txt"), "both\n").unwrap();
    ok(run(&old, repo, &["add", "f.txt"]));
    ok(run(
        &old,
        repo,
        &["commit", "-m", "picked by the older program"],
    ));
    ok(run(&new, repo, &["switch", "feature/side"]));
    let stopped = run(&new, repo, &["cherry-pick", "main~1"]);
    assert_eq!(stopped.status.code(), Some(1));
    std::fs::write(repo.join("f.txt"), "side and main\n").unwrap();
    ok(run(&old, repo, &["add", "f.txt"]));
    ok(run(&new, repo, &["cherry-pick", "--continue"]));
    assert_ne!(ok(run(&new, repo, &["rev-parse", "HEAD"])), head);
    assert!(status().contains(r#""operation":null"#), "{}", status());
    same_reads(&old, repo);
}

/// The record of a whole step is one the older program does not know: it
/// passes it over when it writes, refuses to undo it, and leaves it for the
/// new program to undo.
#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_older_program_refuses_to_undo_a_step_and_works_on() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    diverged(repo);
    let status = || ok(run(&new, repo, &["status", "--json"]));

    // the older program stops the merge, the new one gives it up
    let stopped = run(&old, repo, &["merge", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    let merging = status();
    ok(run(&new, repo, &["merge", "--abort"]));
    let refused = run(&old, repo, &["undo"]);
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(said.contains("automatic undo refused"), "{said}");
    assert!(status().contains(r#""clean":true"#), "{}", status());
    same_reads(&old, repo);

    // the record in the log is in no command's way
    ok(run(&old, repo, &["branch", "kept"]));
    ok(run(&old, repo, &["branch", "-d", "kept"]));
    ok(run(&new, repo, &["fsck"]));

    // what the older program does itself it still undoes
    std::fs::write(repo.join("g.txt"), "g\n").unwrap();
    ok(run(&old, repo, &["add", "g.txt"]));
    ok(run(&old, repo, &["commit", "-m", "by the older program"]));
    ok(run(&old, repo, &["undo"]));
    ok(run(
        &old,
        repo,
        &["commit", "-m", "by the older program, again"],
    ));
    same_reads(&old, repo);

    // a step the new program takes back while it is the last record
    let stopped = run(&new, repo, &["merge", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    ok(run(&new, repo, &["merge", "--abort"]));
    ok(run(&new, repo, &["undo"]));
    assert!(repo.join(".alt/MERGE_HEAD").exists());
    assert!(status().contains(r#""unmerged":["f.txt"]"#), "{}", status());
    assert!(merging.contains(r#""operation":{"kind":"merge"}"#));
    // the older program concludes the merge the undo brought back
    std::fs::write(repo.join("f.txt"), "both\n").unwrap();
    ok(run(&old, repo, &["add", "f.txt"]));
    ok(run(
        &old,
        repo,
        &["commit", "-m", "merged by the older program"],
    ));
    let merged = ok(run(&new, repo, &["cat-file", "-p", "HEAD"]));
    assert_eq!(merged.matches("\nparent ").count(), 2, "{merged}");
    same_reads(&old, repo);
}

/// A merge the older program stopped leaves no record: the step before it
/// is not taken back from under it.
#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_step_is_not_undone_from_under_what_the_older_program_stopped() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    diverged(repo);
    let status = || ok(run(&new, repo, &["status", "--json"]));
    ok(run(&new, repo, &["branch", "feature/other"]));

    // the new program stops a pick and concludes it with a commit
    let stopped = run(&new, repo, &["cherry-pick", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    std::fs::write(repo.join("f.txt"), "picked\n").unwrap();
    ok(run(&new, repo, &["add", "f.txt"]));
    ok(run(&new, repo, &["commit", "-m", "picked"]));
    // the older one stops a merge on top of it
    let stopped = run(&old, repo, &["merge", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    let merging = status();
    let head = ok(run(&new, repo, &["rev-parse", "HEAD"]));

    let refused = run(&new, repo, &["undo"]);
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(said.contains("recovery precondition failed"), "{said}");
    assert_eq!(status(), merging);
    assert_eq!(ok(run(&new, repo, &["rev-parse", "HEAD"])), head);
    assert!(repo.join(".alt/MERGE_HEAD").exists());
    assert!(!repo.join(".alt/CHERRY_PICK_HEAD").exists());
    ok(run(&new, repo, &["merge", "--abort"]));
    same_reads(&old, repo);
}
