//! A command that failed in the middle of its checkout is found by each
//! command after it. How far the log was found to hold nothing open is
//! kept beside the index (`recovery.checked`): neither the command that
//! fails nor one that is refused over what it left moves that past it.

use crate::step_crash::bench::{copy, restore, run};
use crate::step_stuck::{MERGE, diverged, limited, pending, refused, stuck};

#[test]
fn a_command_that_failed_midway_is_found_by_each_command_after_it() {
    let bench = diverged();
    let repo = bench.repo();
    // every file the merge writes is stored for a checkout already
    run(&repo, MERGE, 1);
    run(&repo, &["merge", "--abort"], 0);
    copy(&repo, &bench.kept("began"));
    let checked = || std::fs::read(repo.join(".alt/recovery.checked")).unwrap();
    restore(&repo, &bench.kept("began"));
    let kept = checked();
    stuck(&bench, MERGE);
    assert_eq!(checked(), kept);
    for tried in [&["add", "f.txt"][..], &["commit", "-m", "m"], &["add", "."]] {
        refused(&limited(&repo, tried, None));
        assert_eq!(checked(), kept, "{tried:?}");
    }
    // brought to its end, the log holds nothing open up to the merge
    run(&repo, &["add", "f.txt"], 0);
    assert_eq!(pending(&repo), [""; 0]);
    assert_ne!(checked(), kept);
}
