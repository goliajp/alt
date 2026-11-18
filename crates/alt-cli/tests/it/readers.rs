//! A command that only reads leaves the logs of the repository as it
//! finds them, whatever state a writer, a power cut or an older alt left
//! them in: it cuts none, writes neither marker, nothing into `odb.lock`
//! or `oplog/sync.lock`, and creates no file that is gone. What is there
//! to cut or to bound waits for the next command that writes, and the
//! reader answers as it would after that command. Its caches it does
//! write: the ref snapshot, and index runs over synced records.

mod commands;
mod daemon;
mod runs;
mod states;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use super::unconfirmed::{alt, ok, stderr};
use commands::{APART, COMMANDS, only_reads, reads};
use states::{Base, ENDS, GONE, MARKS, RUNS, Step};

/// The directories of index runs, each beside the log it indexes.
const RUN_DIRS: [&str; 4] = [
    "store/chunks.idx",
    "store/manifests/blobmap.idx",
    "tier1.idx",
    "map.alt.idx",
];

/// What a command that only reads may write under `.alt`: the ref
/// snapshot, a cache of the refs as the log gives them, and index runs.
fn cache(path: &Path) -> bool {
    path == Path::new("refs/snapshot") || RUN_DIRS.iter().any(|dir| path.starts_with(dir))
}

/// Every regular file under `.alt` with its bytes; anything else by name.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap().map(|entry| entry.unwrap()) {
            let (path, kind) = (entry.path(), entry.file_type().unwrap());
            let name = path.strip_prefix(root).unwrap().to_owned();
            if kind.is_dir() {
                out.insert(name, None);
                walk(root, &path, out);
            } else {
                out.insert(name, kind.is_file().then(|| fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&dir.join(".alt"), &dir.join(".alt"), &mut out);
    out
}

/// The paths that came, went or hold other bytes, the caches and what
/// `spared` names left out.
fn changed(
    before: &BTreeMap<PathBuf, Option<Vec<u8>>>,
    after: &BTreeMap<PathBuf, Option<Vec<u8>>>,
    spared: &[&str],
) -> Vec<String> {
    let mut paths: Vec<&PathBuf> = before.keys().chain(after.keys()).collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter(|path| !cache(path) && !spared.iter().any(|spared| path.starts_with(spared)))
        .filter_map(|path| match (before.get(path), after.get(path)) {
            (None, _) => Some(format!("+{}", path.display())),
            (_, None) => Some(format!("-{}", path.display())),
            (Some(Some(was)), Some(Some(is))) if was != is => {
                Some(format!("{} ({} > {})", path.display(), was.len(), is.len()))
            }
            _ => None,
        })
        .collect()
}

/// Runs a command of [`COMMANDS`] in `dir`, `out` being a directory
/// outside the repository that is not there.
fn run(dir: &Path, args: &[&str], out: &Path) -> Output {
    let requests = dir.join(".alt/requests");
    let request = fs::read_dir(requests).unwrap().next().unwrap().unwrap();
    let request = request.file_name().into_string().unwrap();
    let args: Vec<&str> = args
        .iter()
        .map(|arg| match *arg {
            "{request}" => request.as_str(),
            "{out}" => out.to_str().unwrap(),
            arg => arg,
        })
        .collect();
    let ran = alt(dir, &args);
    let _ = fs::remove_dir_all(out);
    ran
}

/// Every command that only reads, run in each of `states`: none changes
/// a file of the store.
fn reads_leave(states: &[(&str, &[Step])]) {
    let base = Base::build();
    let reads = reads();
    let mut broke = Vec::new();
    for (n, (name, steps)) in states.iter().enumerate() {
        let dir = base.state(&format!("s{n}"), steps);
        let out = dir.with_extension("out");
        let mut before = tree(&dir);
        for args in &reads {
            let ran = run(&dir, args, &out);
            let after = tree(&dir);
            let changes = changed(&before, &after, &[]);
            if !changes.is_empty() {
                let said = stderr(&ran);
                broke.push(format!("{name}: alt {args:?} changed {changes:?}: {said}"));
            }
            before = after;
        }
    }
    assert!(broke.is_empty(), "{}", broke.join("\n"));
}

#[test]
fn every_command_is_asked_whether_it_only_reads() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let help = ok(alt(tmp.path(), &["--help"]));
    let listed = help.split("Commands:\n").nth(1).unwrap();
    let listed = listed.split("\n\n").next().unwrap();
    let names: Vec<&str> = listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert!(names.len() > 40, "{help}");
    for name in names {
        let known = COMMANDS.iter().any(|args| args[0] == name) || APART.contains(&name);
        assert!(known, "alt {name} is in no list of this test");
    }
    for reads in [
        "log",
        "show",
        "status",
        "diff",
        "rev-parse",
        "cat-file",
        "blame",
    ] {
        let found = COMMANDS.iter().find(|args| args[0] == reads).unwrap();
        assert!(only_reads(found), "{found:?}");
    }
    for reads in [
        &["branch"][..],
        &["tag"],
        &["op-log"],
        &["fsck"],
        &["export", "x"],
    ] {
        assert!(only_reads(reads), "{reads:?}");
    }
    for writes in [&["add", "a.txt"][..], &["commit", "-m", "x"], &["tag", "x"]] {
        assert!(!only_reads(writes), "{writes:?}");
    }
    assert!(reads().len() >= 50);
}

#[test]
fn a_command_that_only_reads_changes_no_file_whatever_ends_the_logs() {
    reads_leave(ENDS);
}

#[test]
fn a_command_that_only_reads_changes_no_file_whatever_the_markers_say() {
    reads_leave(MARKS);
}

#[test]
fn a_command_that_only_reads_changes_no_file_where_index_runs_are_gone_or_cut() {
    reads_leave(RUNS);
}

#[test]
fn a_command_that_only_reads_creates_no_file_that_is_gone() {
    reads_leave(GONE);
}

/// The number between `before` and `after` in a line a command wrote to
/// stderr.
fn told(out: &Output, before: &str, after: &str) -> Option<usize> {
    let said = stderr(out);
    let mut numbers = said.lines().filter_map(|line| {
        let rest = line.strip_prefix(before)?;
        rest.split_once(after)?.0.parse().ok()
    });
    numbers.next()
}

/// What a command printed, and how it ended.
fn answer(ran: &Output) -> (Option<i32>, String) {
    let said = String::from_utf8_lossy(&ran.stdout).into_owned();
    (ran.status.code(), said)
}

#[test]
fn a_reader_answers_as_it_does_after_the_next_writer_cut() {
    const ASKED: [&[&str]; 6] = [
        &["log"],
        &["rev-parse", "HEAD"],
        &["status"],
        &["cat-file", "-p", "HEAD"],
        &["branch"],
        &["diff", "--cached"],
    ];
    let base = Base::build();
    let (mut cut, mut same, mut left_out) = (0, 0, 0);
    let states = ENDS.iter().chain(MARKS).chain(RUNS);
    for (n, (name, steps)) in states.enumerate() {
        let found = base.state(&format!("found{n}"), steps);
        let written = base.state(&format!("written{n}"), steps);
        // stores an object and moves a ref that none of the answers
        // shows: the logs are read to their ends, cut and bounded
        let wrote = alt(&written, &["tag", "-a", "probe", "-m", "probe"]);
        if !wrote.status.success() {
            // damage: the writer reports it and cuts nothing
            continue;
        }
        cut += usize::from(stderr(&wrote).contains("warning: discarded"));
        let ops = " operation(s) at the end of the operation log";
        let discarded = told(&wrote, "warning: discarded ", ops).unwrap_or(0);
        for args in ASKED {
            let read = alt(&found, args);
            // whole operations go with what the writer cuts: a reader
            // leaves out the ones the writer discards, and says so
            let never = " operation(s) in the operation log were never synced";
            let passed = told(&read, "warning: the last ", never).unwrap_or(0);
            // a command that fails says why and nothing else
            if read.status.success() {
                assert_eq!(passed, discarded, "{name}: alt {args:?}");
                left_out += usize::from(discarded > 0);
            }
            let after = alt(&written, args);
            assert_eq!(answer(&read), answer(&after), "{name}: alt {args:?}");
            same += 1;
        }
    }
    assert!(
        cut >= 20 && same >= 120 && left_out >= 12,
        "{cut} states cut, {same} answers the same, {left_out} of them with operations left out"
    );
}

/// An export is a command that only reads: it writes out the repository
/// as it was before the operations that were never synced and name
/// objects which are not stored, and says so.
#[test]
fn an_export_leaves_out_what_every_reader_leaves_out() {
    let base = Base::build();
    let dir = base.state("exported", &[Step::Lag, Step::PackLost]);
    let head = alt(&dir, &["rev-parse", "HEAD"]);
    assert!(stderr(&head).contains("never synced"), "{head:?}");
    let out = dir.with_extension("git");
    let export = alt(&dir, &["export", out.to_str().unwrap()]);
    assert!(stderr(&export).contains("never synced"), "{export:?}");
    assert!(export.status.success(), "{export:?}");
    let exported = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&out)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert_eq!(answer(&exported), answer(&head));
}
