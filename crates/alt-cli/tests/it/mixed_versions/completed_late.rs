//! A checkout of the older program left unfinished after the mark of a
//! step whose guard a power cut took: the step is left as it is and
//! closed, and the checkout is finished after that. Its completion lies
//! after the record that closed the step, and an undo that finds it last
//! takes it for a record of that step: refused, as the step is.

use std::path::Path;
use std::process::{Command, Output};

use super::step_mark::marked_without_its_guard;
use super::{new_bin, ok, old_bin, run};

const LARGE: usize = 1 << 20;

/// `bin` under a limit of 512 blocks on the files it writes.
fn limited(bin: &Path, repo: &Path, args: &[&str]) -> Output {
    Command::new("sh")
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(["-c", r#"trap "" XFSZ; ulimit -f 512; exec "$0" "$@""#])
        .arg(bin)
        .args(args)
        .output()
        .unwrap()
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_checkout_finished_after_its_step_was_left_as_it_is_is_not_undone_by_itself() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    marked_without_its_guard(repo);
    let said = |o: &Output| String::from_utf8_lossy(&o.stderr).into_owned();
    // a branch whose checkout the older program cannot finish under the limit
    ok(run(&old, repo, &["switch", "-c", "feature/large"]));
    std::fs::write(repo.join("z.txt"), "z".repeat(LARGE)).unwrap();
    ok(run(&old, repo, &["add", "z.txt"]));
    ok(run(&old, repo, &["commit", "-m", "large"]));
    ok(run(&old, repo, &["switch", "main"]));
    let failed = limited(&old, repo, &["switch", "feature/large"]);
    assert!(!failed.status.success(), "{}", said(&failed));
    let pending = ok(run(&old, repo, &["operation", "pending"]));
    assert_eq!(pending.lines().count(), 1, "{pending}");
    let checkout = pending.split(' ').next().unwrap();

    // the new program leaves the step as it is and closes it; the checkout
    // of the older one still waits
    let next = run(&new, repo, &["branch", "kept"]);
    assert_eq!(next.status.code(), Some(75), "{}", said(&next));
    assert!(
        said(&next).contains("it is left as it is"),
        "{}",
        said(&next)
    );
    assert!(said(&next).contains("incomplete"), "{}", said(&next));
    ok(run(&new, repo, &["operation", "recover", checkout]));
    assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "");
    assert_eq!(std::fs::read(repo.join("z.txt")).unwrap().len(), LARGE);

    let found = ok(run(&new, repo, &["status", "--json"]));
    let undone = run(&new, repo, &["undo"]);
    assert!(!undone.status.success());
    let why = "the last operation was interrupted and left as it was";
    assert!(said(&undone).contains(why), "{}", said(&undone));
    assert_eq!(ok(run(&new, repo, &["status", "--json"])), found);
    ok(run(&new, repo, &["fsck"]));
}
