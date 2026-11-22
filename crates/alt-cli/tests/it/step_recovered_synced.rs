//! A step that was cut short is brought to an end synced, whichever
//! command finds it. An add leaves what it writes unsynced; the state
//! file it puts as the log says and the record that closes the step are
//! synced all the same: the next command that syncs the log would
//! otherwise make that record durable beside a state file that is not.

use crate::staged_durability::power_cut;
use crate::step_crash::bench::{Bench, alt, copy, run};
use crate::step_given_up::killed_at;
use crate::sync_order::{log_len, log_synced};

const MERGE_HEAD: &str = ".alt/MERGE_HEAD";

/// A merge that stops on the conflict in `f.txt`, killed after the intent
/// of its checkout was synced and before the file that says a merge is
/// under way was written; kept as `cut`.
fn cut_before_its_state_file() -> Bench {
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
    killed_at(&bench, &["merge", "side"], "state-file:before");
    assert!(!repo.join(MERGE_HEAD).exists());
    bench.write("x.txt", "x\n");
    copy(&repo, &bench.kept("cut"));
    bench
}

/// The merge is under way, with nothing left to recover, and a commit
/// concludes it with both parents.
fn merging(bench: &Bench, when: &str) {
    let repo = bench.repo();
    assert!(repo.join(MERGE_HEAD).exists(), "{when}");
    assert_eq!(run(&repo, &["operation", "pending"], 0), "", "{when}");
    bench.write("f.txt", "both\n");
    run(&repo, &["add", "f.txt", "x.txt"], 0);
    run(&repo, &["commit", "-m", "merged"], 0);
    assert_eq!(bench.shape().head.1, 2, "{when}");
    assert!(alt(&repo, &["fsck"], None).status.success(), "{when}");
}

#[test]
fn a_merge_an_add_brought_to_its_end_is_recorded_as_synced_and_outlives_a_power_cut() {
    let bench = cut_before_its_state_file();
    let repo = bench.repo();
    let alt_dir = repo.join(".alt");
    let cut = log_len(&alt_dir);
    assert_eq!(log_synced(&alt_dir), cut);
    run(&repo, &["add", "x.txt"], 0);
    // the records that end the merge are synced, those of the add are not
    let synced = log_synced(&alt_dir);
    assert!(cut < synced && synced < log_len(&alt_dir), "{cut} {synced}");
    power_cut(&repo);
    assert_eq!(log_len(&alt_dir), synced);
    merging(&bench, "after the power cut");
}

#[cfg(target_os = "linux")]
mod files {
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use crate::step_crash::bench::{restore, said};
    use crate::synced_files::{library, synced};

    const LOG: &str = ".alt/oplog/log";

    fn add(repo: &Path, served: bool, crash: Option<&str>) -> Command {
        let mut command = crate::alt_command();
        command
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "tester")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("XDG_CONFIG_HOME", repo.join(".alt/test-config"))
            .args(["add", "x.txt"]);
        match served {
            true => command.env("ALT_DAEMON_IDLE_MS", "3000"),
            false => command.env("ALT_NO_DAEMON", "1"),
        };
        if let Some(crash) = crash {
            command.env("ALT_CRASH_POINT", crash);
        }
        command
    }

    /// A file written beside the index is synced under the name it is
    /// written by, before it is put in place.
    fn written(path: &str) -> bool {
        path.starts_with(".alt/.tmp")
    }

    #[test]
    fn an_add_that_brings_a_merge_to_its_end_syncs_the_state_file_and_then_the_log() {
        let bench = cut_before_its_state_file();
        let repo = bench.repo();
        let beside = bench.kept("library");
        std::fs::create_dir(&beside).unwrap();
        let library = library(&beside);
        // the first points the add passes are those around the state file:
        // what it synced up to each
        let upto = |point: &str| {
            restore(&repo, &bench.kept("cut"));
            let (killed, files) = synced(&repo, &library, add(&repo, false, Some(point)));
            assert_eq!(killed.status.code(), None, "{}", said(&killed));
            files
        };
        let (before, after) = (upto("1"), upto("2"));
        let wrote = &after[before.len()..];
        assert_eq!(after[..before.len()], before[..], "{after:?}");
        assert!(wrote.len() > 1 && written(&wrote[0]), "{wrote:?}");
        assert_eq!(wrote[1], ".alt", "{wrote:?}");
        assert!(repo.join(MERGE_HEAD).exists());

        for served in [false, true] {
            restore(&repo, &bench.kept("cut"));
            let (added, files) = synced(&repo, &library, add(&repo, served, None));
            assert!(added.status.success(), "{}", said(&added));
            assert_eq!(repo.join(".alt/daemon.meta").exists(), served);
            let file = files.iter().position(|path| written(path));
            let log = files.iter().rposition(|path| path == LOG);
            assert!(file.is_some() && file < log, "served {served}: {files:?}");
            let alt_dir = repo.join(".alt");
            let (synced, len) = (log_synced(&alt_dir), log_len(&alt_dir));
            assert!(synced < len, "served {served}: the add was synced");
            if served {
                // it did not outlive the power cut
                drop(crate::scan_daemon::Stop(
                    crate::scan_daemon::daemon_pid(&repo),
                    false,
                ));
            }
            power_cut(&repo);
            merging(&bench, &format!("served {served}"));
        }
    }
}
