//! A switch run by the daemon leaves the daemon knowing the tree under
//! the new index: the status that follows is answered as a direct run
//! answers it, whatever the switch moved and whatever others did to the
//! tree while it ran.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::scan_daemon::{agree, output, run, served, write};
#[cfg(target_os = "linux")]
use crate::scan_daemon::{daemon_pid, instances};

#[test]
fn a_switch_leaves_directories_shown_as_one_path_as_the_new_index_has_them() {
    let (dir, _stop) = served();
    let root = dir.path();
    run(root, &["switch", "-c", "more"], true);
    write(root, "extra/t.txt", "tracked\n");
    write(root, "only/deep/f.txt", "f\n");
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "more"], false);
    write(root, "extra/u.txt", "untracked\n");
    write(root, "docs/new/n.txt", "n\n");
    let listed = |status: &str| -> Vec<String> {
        let lines = status.lines().filter(|l| l.starts_with('\t'));
        lines.map(|l| l.trim().to_owned()).collect()
    };
    assert_eq!(
        listed(&agree(root, "on more")),
        ["docs/new/", "extra/u.txt"]
    );
    for round in 0..3 {
        // a directory with a tracked file comes to hold none, and is one path
        run(root, &["switch", "main"], true);
        let at = format!("on main, round {round}");
        assert_eq!(listed(&agree(root, &at)), ["docs/new/", "extra/"]);
        // and the other way: the files in it are named again
        run(root, &["switch", "more"], true);
        let at = format!("on more, round {round}");
        assert_eq!(listed(&agree(root, &at)), ["docs/new/", "extra/u.txt"]);
    }
    // an index that only the content of a file differs in
    write(root, "a.txt", "other\n");
    run(root, &["add", "a.txt"], true);
    run(root, &["commit", "-m", "other"], true);
    run(root, &["switch", "main"], true);
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "aaaa\n"
    );
    assert_eq!(listed(&agree(root, "back")), ["docs/new/", "extra/"]);
}

#[test]
fn what_others_do_to_the_tree_while_a_switch_runs_is_in_the_next_status() {
    let (dir, _stop) = served();
    let root = dir.path();
    // on either branch: files a switch leaves alone, beside those it moves
    for d in 0..8 {
        for k in 0..6 {
            write(root, &format!("many/d{d}/steady{k}.txt"), "steady\n");
        }
    }
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "steady"], false);
    run(root, &["switch", "-c", "many"], true);
    for f in 0..120 {
        write(root, &format!("many/d{}/f{f}.txt", f % 8), "x\n");
        write(root, &format!("fresh/e{}/g{f}.txt", f % 5), "y\n");
    }
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "many"], false);
    assert!(agree(root, "committed").contains("nothing to commit"));

    for (round, branch) in ["main", "many", "main", "many"].into_iter().enumerate() {
        let done = AtomicBool::new(false);
        // a switch is refused over a changed file: the others leave the
        // tracked ones alone until it has begun to move files
        let first_moved = root.join("fresh/e0/g0.txt");
        let was_there = first_moved.exists();
        let others = std::thread::scope(|scope| {
            let others = scope.spawn(|| {
                let mut n = 0;
                // at least a few rounds, and for as long as the switch runs
                while n < 8 || !done.load(Ordering::Relaxed) {
                    let (d, k) = (n % 8, n % 6);
                    let steady = format!("many/d{d}/steady{k}.txt");
                    match (first_moved.exists() != was_there, n % 3) {
                        (false, _) => {}
                        (true, 0) => write(root, &steady, &format!("changed {round} {n}\n")),
                        (true, 1) => drop(std::fs::remove_file(root.join(&steady))),
                        (true, _) => write(root, &steady, "steady\n"),
                    }
                    write(root, &format!("many/d{d}/stray{round}-{n}.txt"), "stray\n");
                    write(root, &format!("beside/r{round}/n{n}/s.txt"), "s\n");
                    if n % 4 == 3 {
                        std::fs::remove_dir_all(root.join(format!("beside/r{round}/n{}", n - 1)))
                            .unwrap();
                    }
                    n += 1;
                    // slower as it goes on: a switch that takes minutes on
                    // a busy disk is not buried in files
                    std::thread::sleep(std::time::Duration::from_millis(3 + n / 8));
                }
                n
            });
            let switched = output(root, &["switch", branch], true);
            done.store(true, Ordering::Relaxed);
            let others = others.join().unwrap();
            let said = String::from_utf8_lossy(&switched.stderr);
            assert!(switched.status.success(), "switch {branch}: {said}");
            others
        });
        assert!(others >= 8);
        assert_eq!(root.join("fresh/e3/g3.txt").exists(), branch == "many");
        let status = agree(root, &format!("after the switch to {branch}"));
        assert!(status.contains(&format!("stray{round}-0.txt")), "{status}");
        // once more, with nothing in between
        assert_eq!(status, agree(root, "again"));
        // as committed again, for the next switch
        for d in 0..8 {
            for k in 0..6 {
                write(root, &format!("many/d{d}/steady{k}.txt"), "steady\n");
            }
        }
        let status = agree(root, "restored");
        assert!(
            !status.contains("modified:") && !status.contains("deleted:"),
            "{status}"
        );
    }
}

#[test]
fn a_comparison_that_failed_is_not_answered_from_memory_the_next_time() {
    let (dir, _stop) = served();
    let root = dir.path();
    let config = "[filter \"strict\"]\n\tclean = false\n\trequired = true\n";
    write(root, ".alt/git-import/config", config);
    write(root, ".gitattributes", "*.txt filter=strict\n");
    write(root, "a.txt", "changed\n");
    // the settings were written a while ago when the daemon keeps its answer
    std::thread::sleep(std::time::Duration::from_millis(50));
    for nth in 0..4 {
        let direct = output(root, &["status"], false);
        let served = output(root, &["status"], true);
        assert!(!direct.status.success(), "the filter fails");
        assert_eq!(served.status.code(), direct.status.code(), "run {nth}");
        assert_eq!(
            String::from_utf8_lossy(&served.stderr),
            String::from_utf8_lossy(&direct.stderr),
            "run {nth}"
        );
    }
    std::fs::remove_file(root.join(".gitattributes")).unwrap();
    assert!(agree(root, "without the filter").contains("modified:   a.txt"));
}

#[cfg(target_os = "linux")]
#[test]
fn a_switch_is_followed_by_no_look_at_the_files_it_left_alone() {
    let (dir, _stop) = served();
    let root = dir.path();
    // a filter that leaves a line behind each time a scan reads a file
    let (script, seen) = (root.join(".alt/seen.sh"), root.join(".alt/seen.log"));
    std::fs::write(&script, format!("echo read >> '{}'\ncat\n", seen.display())).unwrap();
    let config = format!("[filter \"seen\"]\n\tclean = sh '{}'\n", script.display());
    write(root, ".alt/git-import/config", &config);
    write(root, ".gitattributes", "kept.txt filter=seen\n");
    for d in 0..6 {
        write(root, &format!("left/d{d}/kept.txt"), "kept\n");
    }
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "left"], false);
    run(root, &["switch", "-c", "moved"], true);
    for f in 0..40 {
        write(root, &format!("moved/e{}/f{f}.txt", f % 4), "x\n");
    }
    write(root, "a.txt", "on moved\n");
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "moved"], false);
    // written in years to come, the file never shows a stat that proves
    // it unchanged: every scan that looks at it reads it
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(300_000_000);
    let kept = std::fs::File::options()
        .append(true)
        .open(root.join("left/d3/kept.txt"));
    let times = std::fs::FileTimes::new().set_modified(later);
    kept.unwrap().set_times(times).unwrap();
    let reads = || std::fs::read_to_string(&seen).unwrap().lines().count();
    assert!(agree(root, "committed").contains("nothing to commit"));

    for branch in ["main", "moved", "main"] {
        // the daemon holds its answer for the tree as it is
        run(root, &["status"], true);
        let before = reads();
        run(root, &["switch", branch], true);
        let status = run(root, &["status"], true);
        let diff = run(root, &["diff"], true);
        assert_eq!(
            reads(),
            before,
            "the switch to {branch} moved nothing there"
        );
        assert_eq!(root.join("moved/e0/f0.txt").exists(), branch == "moved");
        // a scan of the whole tree does read it
        assert_eq!(status, run(root, &["status"], false), "on {branch}");
        assert_eq!(reads(), before + 1);
        assert_eq!(diff, run(root, &["diff"], false), "on {branch}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_switch_of_more_files_than_the_queue_holds_events_for_loses_none() {
    let (dir, stop) = served();
    let root = dir.path();
    // the directories are there, and watched, on either branch
    for d in 0..30 {
        write(root, &format!("many/d{d}/kept.txt"), "kept\n");
    }
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "directories"], false);
    run(root, &["switch", "-c", "many"], true);
    for f in 0..6000 {
        write(root, &format!("many/d{}/f{f}.txt", f % 30), "x\n");
    }
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "many"], false);
    assert!(agree(root, "committed").contains("nothing to commit"));
    let held = instances(stop.0);
    assert_eq!(held.len(), 1);
    for round in 0..2 {
        run(root, &["switch", "main"], true);
        assert!(!root.join("many/d7/f7.txt").exists());
        assert!(agree(root, "without them").contains("nothing to commit"));
        run(root, &["switch", "many"], true);
        write(root, "many/d7/f7.txt", &format!("changed {round}\n"));
        assert!(agree(root, "with them").contains("modified:   many/d7/f7.txt"));
        write(root, "many/d7/f7.txt", "x\n");
        assert!(agree(root, "restored").contains("nothing to commit"));
    }
    // a queue that ran over would have been replaced by another
    assert_eq!(instances(stop.0), held);
    assert_eq!(daemon_pid(root), stop.0);
}
