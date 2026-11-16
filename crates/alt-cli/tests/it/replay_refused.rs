//! What a rebase, a revert, a merge or a cherry-pick may not do is refused
//! before any of it is done, and no step one of them goes on with commits
//! what the policy leaves out.

use std::path::Path;
use std::process::Output;

use crate::capability_gate::{alt, alt_as, fails_with, fixture, ok, write_policy};

const POLICY: &str = "agent:* -> path=src/**\n";

fn write(repo: &Path, file: &str, body: &str) {
    std::fs::create_dir_all(repo.join(file).parent().unwrap()).unwrap();
    std::fs::write(repo.join(file), body).unwrap();
}

fn commit(repo: &Path, message: &str) {
    ok(alt(repo, &["add", "."]));
    ok(alt(repo, &["commit", "-m", message]));
}

/// The command stopped at a conflict.
fn stops(o: Output) {
    assert_eq!(o.status.code(), Some(1), "{o:?}");
}

/// What the commands say of the tree, the refs, the index, the files and
/// the sequences under way.
fn state(repo: &Path, refs: &[&str]) -> (Vec<String>, Vec<Vec<u8>>, Vec<bool>) {
    let mut said: Vec<String> = ["status", "branch"].map(|c| ok(alt(repo, &[c]))).into();
    said.extend(refs.iter().map(|r| ok(alt(repo, &["rev-parse", r]))));
    let files = [".alt/index", "src/b.rs", "scripts/run.sh", "scripts/old.sh"];
    let files = files.map(|f| std::fs::read(repo.join(f)).unwrap_or_default());
    let under_way = ["MERGE_HEAD", "REVERT_STATE", "REBASE_STATE"];
    let under_way = under_way.map(|f| repo.join(".alt").join(f).exists());
    (said, files.into(), under_way.into())
}

/// `main` and `topic` change the same line of `src/b.rs`; `topic` goes on
/// with a second change of it. Left on `topic`.
fn diverged(tmp: &alt_os::temp::TempDir) -> &Path {
    let repo = fixture(tmp);
    write(repo, "src/b.rs", "l1\nl2\nl3\n");
    write(repo, "scripts/old.sh", "old\n");
    write(repo, "scripts/run.sh", "run\n");
    commit(repo, "files");
    ok(alt(repo, &["switch", "-c", "topic"]));
    write(repo, "src/b.rs", "l1\ntopic\nl3\n");
    commit(repo, "topic: a line");
    write(repo, "src/b.rs", "l1\nlater\nl3\n");
    commit(repo, "topic: the line again");
    ok(alt(repo, &["switch", "main"]));
    write(repo, "src/b.rs", "l1\nmain\nl3\n");
    commit(repo, "main: the line");
    ok(alt(repo, &["switch", "topic"]));
    write_policy(repo, POLICY);
    repo
}

/// The restricted principal resolves the conflict a rebase or a revert
/// stopped at, another stages a change out of tree, and the first goes on:
/// refused, with the sequence as it stood, for the other to abort. Without
/// what the other staged it goes on.
#[test]
fn a_stopped_sequence_goes_on_with_no_change_another_staged_out_of_tree() {
    for sequence in [&["rebase", "main"][..], &["revert", "HEAD~1"]] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let repo = diverged(&tmp);
        let verb = sequence[0];
        let before = state(repo, &["topic", "main"]);
        for staged_by_another in [true, false] {
            stops(alt_as(repo, sequence, "bot"));
            write(repo, "src/b.rs", "l1\ntopic\nl3\n");
            ok(alt_as(repo, &["add", "src/b.rs"], "bot"));
            if !staged_by_another {
                ok(alt_as(repo, &[verb, "--continue"], "bot"));
                let shown = ok(alt(repo, &["show", "topic"]));
                assert!(!shown.contains("scripts/"), "{verb}: {shown}");
                continue;
            }
            write(repo, "scripts/old.sh", "another's\n");
            ok(alt(repo, &["add", "scripts/old.sh"]));
            let stopped = state(repo, &["topic", "main"]);
            fails_with(
                alt_as(repo, &[verb, "--continue"], "bot"),
                "capability denied: path 'scripts/old.sh'",
            );
            // nor is what the other staged there the restricted one's to drop
            fails_with(alt_as(repo, &[verb, "--abort"], "bot"), "capability denied");
            assert_eq!(state(repo, &["topic", "main"]), stopped, "{verb}");
            ok(alt(repo, &[verb, "--abort"]));
            assert_eq!(state(repo, &["topic", "main"]), before, "{verb}");
        }
    }
}
