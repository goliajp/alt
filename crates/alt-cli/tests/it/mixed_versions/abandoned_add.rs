//! An add the older program takes for unfinished.

use std::path::Path;

use super::*;

/// The one operation `bin` lists as waiting for a recovery.
fn pending(bin: &Path, repo: &Path) -> String {
    let listed = ok(run(bin, repo, &["operation", "pending"]));
    listed.split_whitespace().next().unwrap().to_owned()
}

/// `bin` refuses to write, asking for a recovery of `op`, and changes
/// neither the index nor what `HEAD` names.
fn refuses_to_write(bin: &Path, repo: &Path, op: &str) {
    let index = fs::read(repo.join(".alt/index")).unwrap();
    let head = ok(run(bin, repo, &["rev-parse", "HEAD"]));
    fs::write(repo.join("refused.txt"), "refused\n").unwrap();
    for args in [&["add", "refused.txt"][..], &["commit", "-m", "refused"]] {
        let out = run(bin, repo, args);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "alt {args:?}");
        let asked = format!("operation {op} has no durable completion; recovery required");
        assert!(said.contains(&asked), "alt {args:?}: {said}");
    }
    fs::remove_file(repo.join("refused.txt")).unwrap();
    assert_eq!(fs::read(repo.join(".alt/index")).unwrap(), index);
    assert_eq!(ok(run(bin, repo, &["rev-parse", "HEAD"])), head);
}

/// This build closes an add it cannot complete with a record the older
/// program does not know. The older one reads the repository as this
/// build does and takes the add for unfinished: it refuses to write, and
/// recovers the add only onto the index from before it, which this build
/// then reports and puts right. Nothing either of them stored is lost.
#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_older_program_takes_an_abandoned_add_for_unfinished_and_damages_nothing() {
    let (old, new) = (old_bin(), new_bin());

    // abandoned by a command that leaves the index as before the add
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path();
    crate::abandoned_add::add_cut_short(repo, false);
    let op = pending(&new, repo);
    ok(run(&new, repo, &["tag", "v1"]));
    assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "");
    same_reads(&old, repo);
    assert_eq!(pending(&old, repo), op);
    refuses_to_write(&old, repo, &op);
    // the older recovery applies the add: the index names the lost blob
    ok(run(&old, repo, &["operation", "recover", &op]));
    assert_eq!(ok(run(&old, repo, &["operation", "pending"])), "");
    let head = ok(run(&old, repo, &["rev-parse", "HEAD"]));
    assert!(!run(&old, repo, &["commit", "-m", "lost"]).status.success());
    assert_eq!(ok(run(&new, repo, &["rev-parse", "HEAD"])), head);
    let checked = run(&new, repo, &["fsck"]);
    let said = String::from_utf8_lossy(&checked.stderr);
    assert!(!checked.status.success());
    assert!(said.contains("index entry new.txt names blob "), "{said}");
    assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "");
    ok(run(&new, repo, &["add", "new.txt"]));
    ok(run(&new, repo, &["commit", "-m", "second"]));
    same_reads(&old, repo);
    commit_files(&old, repo, "old", 3);
    commit_files(&new, repo, "new", 3);
    same_reads(&old, repo);
    assert_eq!(head_tree(&new, repo).lines().count(), 8);

    // abandoned, and this build has changed the index since
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path();
    crate::abandoned_add::add_cut_short(repo, false);
    let op = pending(&new, repo);
    commit_files(&new, repo, "new-a", 3);
    same_reads(&old, repo);
    refuses_to_write(&old, repo, &op);
    let recovery = run(&old, repo, &["operation", "recover", &op]);
    let said = String::from_utf8_lossy(&recovery.stderr);
    assert!(!recovery.status.success());
    assert!(
        said.contains("index matches neither recorded state"),
        "{said}"
    );
    refuses_to_write(&old, repo, &op);
    same_reads(&old, repo);
    commit_files(&new, repo, "new-b", 3);
    same_reads(&old, repo);
    assert_eq!(head_tree(&new, repo).lines().count(), 8);
}
