//! What is left as the user made it where a merge that was cut short is
//! brought to an end or given up: a conflict file passed over is named
//! with where the other side of it is, giving up with `--keep-changes` is
//! refused before it writes where a file it leaves is in the way, and one
//! that was itself cut short ends as one that was not.

use std::path::Path;
use std::process::Output;

use crate::step_crash::bench::{Bench, alt, copy, restore, run, said};

/// `main` and `side` part after one commit: both change `f.txt`; `side`
/// alone changes `t.txt`, and puts a file `d` where the directory of
/// `d/x` was.
fn diverged(bench: &Bench) {
    bench.write("f.txt", "1\n2\n3\n");
    bench.write("t.txt", "t\n");
    bench.write("d/x", "x\n");
    bench.commit("files");
    let repo = bench.repo();
    run(&repo, &["switch", "-c", "side"], 0);
    bench.write("f.txt", "1\nside\n3\n");
    bench.write("t.txt", "t by side\n");
    std::fs::remove_dir_all(repo.join("d")).unwrap();
    bench.write("d", "a file\n");
    bench.commit("side");
    run(&repo, &["switch", "main"], 0);
    bench.write("f.txt", "1\nmain\n3\n");
    bench.commit("main");
    copy(&repo, &bench.kept("fixture"));
}

fn bench() -> Bench {
    let bench = Bench::new();
    run(&bench.repo(), &["init", "."], 0);
    diverged(&bench);
    bench
}

/// The points `command` passes in the workspace as it is, which is then
/// put back as it was.
fn points(bench: &Bench, command: &[&str]) -> Vec<String> {
    let (repo, list, was) = (bench.repo(), bench.kept("points"), bench.kept("was"));
    let _ = std::fs::remove_file(&list);
    restore(&was, &repo);
    alt(&repo, command, Some(&format!("list:{}", list.display())));
    restore(&repo, &was);
    let points = std::fs::read_to_string(&list).unwrap();
    points.lines().map(str::to_owned).collect()
}

/// The merge killed the first time it passes `point`. The operation that
/// waits.
fn merge_killed_at(bench: &Bench, point: &str) -> String {
    let repo = bench.repo();
    restore(&repo, &bench.kept("fixture"));
    let merge = ["merge", "side"];
    let at = points(bench, &merge).iter().position(|p| p == point);
    let killed = alt(&repo, &merge, Some(&(at.unwrap() + 1).to_string()));
    assert_eq!(killed.status.code(), None, "{}", said(&killed));
    let pending = run(&repo, &["operation", "pending"], 0);
    assert!(!pending.is_empty(), "nothing waits after {point}");
    pending.split(' ').next().unwrap().to_owned()
}

fn text(repo: &Path, file: &str) -> String {
    std::fs::read_to_string(repo.join(file)).unwrap()
}

fn log(repo: &Path) -> Vec<u8> {
    std::fs::read(repo.join(".alt/oplog/log")).unwrap()
}

/// A command that writes and here finds nothing to write: it brings what
/// was cut short to an end first.
fn next(repo: &Path) -> Output {
    alt(repo, &["branch", "-d", "no-such-branch"], None)
}

/// The conflict file that was passed over is named, with the command that
/// prints what the other side has there.
fn named_with_the_other_side(repo: &Path, told: &str) {
    let named = "warning: 'f.txt' was changed after the operation was interrupted and is left \
                 as it is, in conflict";
    assert!(told.contains(named), "{told}");
    assert!(told.contains("it may hold no conflict markers"), "{told}");
    let (_, shown) = told.split_once("`alt cat-file -p ").expect(told);
    let blob = shown.split('`').next().unwrap();
    assert_eq!(run(repo, &["cat-file", "-p", blob], 0), "1\nside\n3\n");
}

#[test]
fn a_conflict_file_left_as_it_was_changed_is_named_with_where_the_other_side_is() {
    let bench = bench();
    let repo = bench.repo();
    // what was changed is the file from before the merge: no side of the
    // conflict is in it
    merge_killed_at(&bench, "checkout-file:before");
    bench.write("f.txt", "mine\n");
    let brought = next(&repo);
    named_with_the_other_side(&repo, &said(&brought));
    assert_eq!(text(&repo, "f.txt"), "mine\n");
    assert_eq!(text(&repo, "t.txt"), "t by side\n");
    // said once: nothing waits any more
    assert!(!said(&next(&repo)).contains("warning"));

    // the merge is not taken back over the file, and says which it is
    let found = bench.shape();
    let undone = alt(&repo, &["undo"], None);
    assert!(!undone.status.success(), "{}", said(&undone));
    let why = "working-tree path 'f.txt' changed";
    assert!(said(&undone).contains(why), "{}", said(&undone));
    assert_eq!(bench.shape(), found);
    assert_eq!(text(&repo, "f.txt"), "mine\n");
}

#[test]
fn a_conflict_file_removed_or_made_a_link_or_a_directory_is_written_kept_or_in_the_way() {
    let bench = bench();
    let repo = bench.repo();
    let conflict = "<<<<<<<";

    // gone: there is nothing to lose, and it is written
    merge_killed_at(&bench, "checkout-index:before");
    assert!(text(&repo, "f.txt").contains(conflict));
    std::fs::remove_file(repo.join("f.txt")).unwrap();
    let brought = next(&repo);
    assert!(!said(&brought).contains("warning"), "{}", said(&brought));
    assert!(text(&repo, "f.txt").contains(conflict));
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");

    // a link: kept as what the user made of the conflict
    merge_killed_at(&bench, "checkout-index:before");
    std::fs::remove_file(repo.join("f.txt")).unwrap();
    std::os::unix::fs::symlink("t.txt", repo.join("f.txt")).unwrap();
    let brought = next(&repo);
    named_with_the_other_side(&repo, &said(&brought));
    let link = std::fs::read_link(repo.join("f.txt")).unwrap();
    assert_eq!(link, Path::new("t.txt"));
    assert_eq!(text(&repo, "t.txt"), "t by side\n");
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");
    let status = run(&repo, &["status", "--json"], 0);
    assert!(status.contains(r#""unmerged":["f.txt"]"#), "{status}");

    // a directory that holds a file: in the way, and nothing is written
    // into it; out of the way, the file is written
    merge_killed_at(&bench, "checkout-file:before");
    std::fs::remove_file(repo.join("f.txt")).unwrap();
    bench.write("f.txt/keep", "mine\n");
    let stuck = next(&repo);
    let told = said(&stuck);
    assert!(told.contains("cannot be brought to an end"), "{told}");
    assert!(told.contains("f.txt"), "{told}");
    assert_eq!(text(&repo, "f.txt/keep"), "mine\n");
    assert_ne!(run(&repo, &["operation", "pending"], 0), "");
    std::fs::rename(repo.join("f.txt"), repo.join("aside")).unwrap();
    next(&repo);
    assert!(text(&repo, "f.txt").contains(conflict));
    assert_eq!(text(&repo, "aside/keep"), "mine\n");
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");
}

#[test]
fn a_file_kept_where_the_merge_found_a_directory_refuses_giving_up_before_it_writes() {
    let bench = bench();
    let repo = bench.repo();
    let id = merge_killed_at(&bench, "checkout-index:before");
    assert_eq!(text(&repo, "d"), "a file\n");
    bench.write("d", "mine\n");
    let (found, written) = (bench.shape(), log(&repo));

    let keep = ["operation", "abandon", &id, "--keep-changes"];
    let refused = alt(&repo, &keep, None);
    let told = said(&refused);
    assert!(!refused.status.success(), "{told}");
    let why = "'d' was changed after the operation was interrupted, and left as it is it is in \
               the way of 'd/x', which the operation found there: move 'd' aside";
    assert!(told.contains(why), "{told}");
    assert_eq!(bench.shape(), found);
    assert_eq!(log(&repo), written, "a record was written");

    // aside, the merge is given up and the directory is back
    std::fs::rename(repo.join("d"), repo.join("mine")).unwrap();
    run(&repo, &keep, 0);
    assert_eq!(text(&repo, "d/x"), "x\n");
    assert_eq!(text(&repo, "mine"), "mine\n");
    assert_eq!(text(&repo, "f.txt"), "1\nmain\n3\n");
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");
    assert!(alt(&repo, &["fsck"], None).status.success());
}

/// A merge cut short with every file written, then `t.txt` changed and the
/// index made one the merge never recorded: it has `staged.txt` in it.
/// Kept as `stuck`. The operation that waits.
fn stuck_with_an_index_of_its_own(bench: &Bench) -> String {
    let repo = bench.repo();
    restore(&repo, &bench.kept("fixture"));
    bench.write("staged.txt", "staged\n");
    run(&repo, &["add", "staged.txt"], 0);
    let index = std::fs::read(repo.join(".alt/index")).unwrap();
    run(&repo, &["undo"], 0);
    std::fs::remove_file(repo.join("staged.txt")).unwrap();
    copy(&repo, &bench.kept("fixture"));

    let id = merge_killed_at(bench, "checkout-index:before");
    bench.write("t.txt", "mine\n");
    std::fs::write(repo.join(".alt/index"), index).unwrap();
    restore(&bench.kept("stuck"), &repo);
    id
}

#[test]
fn giving_up_with_changes_kept_that_was_cut_short_ends_as_one_that_was_not() {
    let bench = bench();
    let repo = bench.repo();
    let id = stuck_with_an_index_of_its_own(&bench);
    let stuck = bench.shape();
    let keep = ["operation", "abandon", &id, "--keep-changes"];
    let passed = points(&bench, &keep);
    assert!(passed.iter().any(|p| p == "checkout-file:before"));

    let whole = alt(&repo, &keep, None);
    assert!(whole.status.success(), "{}", said(&whole));
    let left = "left as it is: t.txt, the index\n";
    assert!(said(&whole).ends_with(left), "{}", said(&whole));
    let given_up = bench.shape();
    assert_ne!(given_up, stuck);
    assert_eq!(text(&repo, "t.txt"), "mine\n");
    assert_eq!(text(&repo, "f.txt"), "1\nmain\n3\n");
    assert_eq!(text(&repo, "d/x"), "x\n");

    let mut ended_by_the_next = 0;
    for (at, point) in passed.iter().enumerate() {
        let when = format!("killed at point {} ({point})", at + 1);
        let at = (at + 1).to_string();
        // run again, it ends where the one that was not cut short does
        restore(&repo, &bench.kept("stuck"));
        let killed = alt(&repo, &keep, Some(&at));
        assert_eq!(killed.status.code(), None, "{when}: {}", said(&killed));
        let again = alt(&repo, &keep, None);
        assert_eq!(bench.shape(), given_up, "{when}: {}", said(&again));
        assert_eq!(run(&repo, &["operation", "pending"], 0), "", "{when}");
        assert!(alt(&repo, &["fsck"], None).status.success(), "{when}");

        // the next command that writes ends it the same way where it was
        // given up in the log, and where it was not the merge still waits
        restore(&repo, &bench.kept("stuck"));
        alt(&repo, &keep, Some(&at));
        next(&repo);
        let found = bench.shape();
        match run(&repo, &["operation", "pending"], 0).is_empty() {
            true => {
                assert_eq!(found, given_up, "{when}");
                ended_by_the_next += 1;
            }
            false => assert_eq!(found, stuck, "{when}"),
        }
    }
    assert!(ended_by_the_next > 1, "{passed:?}");
}

#[test]
fn a_merge_in_a_sha256_repository_is_brought_to_an_end_and_given_up() {
    let bench = Bench::new();
    let (repo, source) = (bench.repo(), bench.kept("source"));
    std::fs::create_dir(&source).unwrap();
    let git = |args: &[&str]| {
        let mut git = std::process::Command::new("git");
        git.current_dir(&source).args(args);
        for who in ["AUTHOR", "COMMITTER"] {
            git.env(format!("GIT_{who}_NAME"), "tester");
            git.env(format!("GIT_{who}_EMAIL"), "t@e");
        }
        assert!(git.status().unwrap().success(), "{args:?}");
    };
    git(&["init", "-q", "-b", "main", "--object-format=sha256"]);
    std::fs::write(source.join("k.txt"), "k\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "k"]);
    std::fs::remove_dir(&repo).unwrap();
    run(&source, &["import", repo.to_str().unwrap()], 0);
    diverged(&bench);
    let before = bench.shape();
    assert_eq!(run(&repo, &["rev-parse", "HEAD"], 0).trim().len(), 64);

    // given up, all is as before the merge
    let id = merge_killed_at(&bench, "checkout-file:before");
    run(&repo, &["operation", "abandon", &id], 0);
    assert_eq!(bench.shape(), before);
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");

    // brought to an end around the conflict file that was changed
    merge_killed_at(&bench, "checkout-index:before");
    bench.write("f.txt", "mine\n");
    let brought = next(&repo);
    named_with_the_other_side(&repo, &said(&brought));
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");
    run(&repo, &["add", "f.txt"], 0);
    run(&repo, &["commit", "-m", "merged"], 0);
    let merged = run(&repo, &["cat-file", "-p", "HEAD"], 0);
    assert_eq!(merged.matches("\nparent ").count(), 2, "{merged}");
    assert!(alt(&repo, &["fsck"], None).status.success());
}

/// Through the daemon of `repo`.
fn served(repo: &Path, args: &[&str]) -> Output {
    crate::scan_daemon::output(repo, args, true)
}

#[test]
fn the_daemon_names_a_conflict_file_it_leaves_and_ends_a_giving_up_with_changes_kept() {
    let bench = bench();
    let repo = bench.repo();
    merge_killed_at(&bench, "checkout-file:before");
    bench.write("f.txt", "mine\n");
    // the undo brings the merge to an end first, and is then refused
    let brought = served(&repo, &["undo"]);
    let stop = crate::scan_daemon::Stop(crate::scan_daemon::daemon_pid(&repo), false);
    named_with_the_other_side(&repo, &said(&brought));
    assert!(!brought.status.success(), "{}", said(&brought));
    assert_eq!(text(&repo, "f.txt"), "mine\n");
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");
    drop(stop);

    // giving up is no command the daemon serves: one cut short as it puts
    // the last file back is ended by the next command, which it does serve
    let id = merge_killed_at(&bench, "checkout-index:before");
    bench.write("t.txt", "mine\n");
    let keep = ["operation", "abandon", &id, "--keep-changes"];
    let passed = points(&bench, &keep);
    let last = passed.iter().rposition(|p| p == "checkout-file:before");
    let killed = alt(&repo, &keep, Some(&(last.unwrap() + 1).to_string()));
    assert_eq!(killed.status.code(), None, "{}", said(&killed));
    let ended = served(&repo, &["add", "t.txt"]);
    let stop = crate::scan_daemon::Stop(crate::scan_daemon::daemon_pid(&repo), false);
    assert!(ended.status.success(), "{}", said(&ended));
    assert_eq!(text(&repo, "t.txt"), "mine\n");
    assert_eq!(text(&repo, "f.txt"), "1\nmain\n3\n");
    assert_eq!(text(&repo, "d/x"), "x\n");
    assert!(!repo.join(".alt/MERGE_HEAD").exists());
    assert_eq!(run(&repo, &["operation", "pending"], 0), "");
    drop(stop);
}
