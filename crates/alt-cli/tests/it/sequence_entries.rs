//! Every way into a revert, a cherry-pick, a rebase or a merge that
//! stopped on a conflict, with every kind of change that waits in the
//! index and in the files, beside git: the command ends as git's ends,
//! and leaves the branch, the index and the files as git leaves them.

mod quit;
mod refused;

use std::process::Output;

use crate::like_git::{Pair, alt, files, git, heads, index, ok};

const COMMANDS: [&str; 4] = ["revert", "cherry-pick", "rebase", "merge"];
const ENTRIES: [&str; 4] = ["continue", "skip", "abort", "quit"];

/// What is done about the conflict before the command is gone back to.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Resolved {
    /// nothing: the conflict is as the command left it
    No,
    /// resolved and staged
    Staged,
    /// resolved and committed by a commit of the user
    Committed,
}

/// What waits beside the conflict when the command is gone back to.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Waits {
    Nothing,
    /// a change that is not staged, in the file the next step writes
    Unstaged,
    /// the same change, staged
    Staged,
    /// a new file, staged
    NewStaged,
    /// a change that is not staged, in a file no step writes
    Elsewhere,
}

const F: &str = "l1\nl2\nl3\n";

/// A pair where `command` has stopped on a conflict in `f`; a revert
/// and a rebase have a step left that writes `other`.
fn stopped(command: &str) -> Pair {
    let pair = Pair::new();
    pair.write("f", F);
    pair.write("other", "base\n");
    pair.write("kept", "base\n");
    pair.commit_all("base");
    let line = |first: &str| F.replacen("l1", first, 1);
    let start: Vec<&str> = match command {
        "revert" => {
            pair.write("f", &line("A1"));
            pair.commit_all("A");
            pair.write("other", "B\n");
            pair.commit_all("B");
            pair.write("f", &line("X1"));
            pair.commit_all("C");
            vec!["revert", "HEAD~2", "HEAD~1"]
        }
        "rebase" => {
            pair.both(&["switch", "-c", "topic"]);
            pair.write("f", &line("T1"));
            pair.commit_all("t1");
            pair.write("other", "T\n");
            pair.commit_all("t2");
            pair.both(&["switch", "main"]);
            pair.write("f", &line("M1"));
            pair.commit_all("m1");
            pair.both(&["switch", "topic"]);
            vec!["rebase", "main"]
        }
        _ => {
            pair.both(&["switch", "-c", "side"]);
            pair.write("f", &line("S1"));
            pair.commit_all("s");
            pair.both(&["switch", "main"]);
            pair.write("f", &line("M1"));
            pair.commit_all("m");
            vec![command, "side"]
        }
    };
    let (ours, theirs) = pair.run(&start);
    assert_eq!(
        (ours.status.code(), theirs.status.code()),
        (Some(1), Some(1)),
        "{command}: {}",
        String::from_utf8_lossy(&ours.stderr)
    );
    pair
}

fn prepare(pair: &Pair, resolved: Resolved, waits: Waits) {
    if resolved != Resolved::No {
        pair.write("f", &F.replacen("l1", "R", 1));
        pair.both(&["add", "f"]);
    }
    if resolved == Resolved::Committed {
        pair.commit_all("resolved");
    }
    match waits {
        Waits::Nothing => {}
        Waits::Unstaged => pair.write("other", "mine\n"),
        Waits::Staged => {
            pair.write("other", "mine\n");
            pair.both(&["add", "other"]);
        }
        Waits::Elsewhere => pair.write("kept", "mine\n"),
        Waits::NewStaged => {
            pair.write("z", "new\n");
            pair.both(&["add", "z"]);
        }
    }
}

/// Whether a command that stopped waits to be gone on with.
fn in_progress(pair: &Pair) -> (bool, bool) {
    let ours = [
        "REVERT_STATE",
        "CHERRY_PICK_HEAD",
        "MERGE_HEAD",
        "REBASE_STATE",
    ];
    let theirs = [
        "sequencer",
        "REVERT_HEAD",
        "CHERRY_PICK_HEAD",
        "MERGE_HEAD",
        "rebase-merge",
    ];
    (
        ours.iter().any(|f| pair.alt.join(".alt").join(f).exists()),
        theirs
            .iter()
            .any(|f| pair.git.join(".git").join(f).exists()),
    )
}

fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.lines().next().unwrap_or_default().to_owned()
}

fn at(pair: &Pair) -> (String, String) {
    (
        ok(alt(&pair.alt, &["rev-parse", "HEAD"])),
        ok(git(&pair.git, &["rev-parse", "HEAD"])),
    )
}

/// The index and the files are the same in both.
fn same_tree(pair: &Pair, cell: &str) {
    let theirs = ok(git(&pair.git, &["ls-files", "-s"]));
    assert_eq!(index(&pair.alt), theirs, "{cell}: index");
    assert_eq!(files(&pair.alt), files(&pair.git), "{cell}: files");
}

/// Both ended alike and left the same behind.
fn same(pair: &Pair, cell: &str, ours: &Output, theirs: &Output) {
    let said = String::from_utf8_lossy(&ours.stderr).into_owned();
    assert_eq!(ours.status.code(), theirs.status.code(), "{cell}: {said}");
    let (here, there) = in_progress(pair);
    assert_eq!(here, there, "{cell}: in progress");
    same_tree(pair, cell);
    let (here, there) = at(pair);
    assert_eq!(here, there, "{cell}: HEAD");
    if !in_progress(pair).1 {
        assert_eq!(heads(&pair.alt), heads(&pair.git), "{cell}: branches");
    }
    let theirs = first_line(&theirs.stderr);
    // a refusal is worded as git words it
    if ours.status.code() == Some(128) {
        assert_eq!(first_line(&ours.stderr), theirs, "{cell}: said");
    }
}

fn cell(command: &str, entry: &str, resolved: Resolved, waits: Waits) {
    let cell = format!("{command} --{entry}, {resolved:?}, {waits:?}");
    let pair = stopped(command);
    prepare(&pair, resolved, waits);
    let before = (index(&pair.alt), files(&pair.alt), at(&pair).0);
    let flag = format!("--{entry}");
    let ours = alt(&pair.alt, &[command, &flag]);
    // the message of a commit git makes here is taken as it stands
    let theirs = git(&pair.git, &["-c", "core.editor=true", command, &flag]);
    let said = String::from_utf8_lossy(&ours.stderr).into_owned();
    let unstaged = matches!(waits, Waits::Unstaged | Waits::Elsewhere);
    if command == "rebase" && entry == "skip" && unstaged {
        // git drops the change; alt keeps it and says how to go on
        assert_eq!(ours.status.code(), Some(1), "{cell}: {said}");
        assert!(
            said.contains("alt stash") && said.contains("alt restore"),
            "{cell}: {said}"
        );
        let after = (index(&pair.alt), files(&pair.alt), at(&pair).0);
        assert_eq!(after, before, "{cell}");
        assert!(in_progress(&pair).0, "{cell}");
        return;
    }
    if command == "rebase" && entry == "quit" {
        // git leaves HEAD at the commits made, on no branch; alt has no
        // such HEAD and goes back to the branch, which did not move
        assert_eq!(ours.status.code(), Some(0), "{cell}: {said}");
        assert_eq!(in_progress(&pair), (false, false), "{cell}");
        same_tree(&pair, &cell);
        let branch = ok(git(&pair.git, &["rev-parse", "topic"]));
        assert_eq!(at(&pair).0, branch, "{cell}");
        let told = String::from_utf8_lossy(&ours.stdout).into_owned();
        assert!(told.contains(before.2.trim()), "{cell}: {told}");
        return;
    }
    same(&pair, &cell, &ours, &theirs);
}

fn cells(command: &str, entry: &str) {
    for resolved in [Resolved::No, Resolved::Staged, Resolved::Committed] {
        for waits in [
            Waits::Nothing,
            Waits::Unstaged,
            Waits::Staged,
            Waits::NewStaged,
            Waits::Elsewhere,
        ] {
            cell(command, entry, resolved, waits);
        }
    }
}

macro_rules! entries {
    ($($name:ident: $command:expr, $entry:expr;)*) => {$(
        #[test]
        fn $name() {
            assert!(COMMANDS.contains(&$command) && ENTRIES.contains(&$entry));
            cells($command, $entry);
        }
    )*};
}

entries! {
    a_revert_is_gone_on_with_as_by_git: "revert", "continue";
    a_revert_is_skipped_as_by_git: "revert", "skip";
    a_revert_is_given_up_as_by_git: "revert", "abort";
    a_revert_is_forgotten_as_by_git: "revert", "quit";
    a_cherry_pick_is_gone_on_with_as_by_git: "cherry-pick", "continue";
    a_cherry_pick_is_skipped_as_by_git: "cherry-pick", "skip";
    a_cherry_pick_is_given_up_as_by_git: "cherry-pick", "abort";
    a_cherry_pick_is_forgotten_as_by_git: "cherry-pick", "quit";
    a_rebase_is_gone_on_with_as_by_git: "rebase", "continue";
    a_rebase_is_skipped_as_by_git_but_for_what_is_not_staged: "rebase", "skip";
    a_rebase_is_given_up_as_by_git: "rebase", "abort";
    a_rebase_is_forgotten_on_its_branch: "rebase", "quit";
    a_merge_is_gone_on_with_as_by_git: "merge", "continue";
    a_merge_is_given_up_as_by_git: "merge", "abort";
    a_merge_is_forgotten_as_by_git: "merge", "quit";
}

/// A revert gone on with after the user committed its first step stops
/// on the next conflict; given up then, it goes back to where it began.
#[test]
fn a_revert_that_stopped_again_is_given_up_from_where_it_began() {
    let pair = Pair::new();
    pair.write("f", F);
    pair.write("g", F);
    pair.commit_all("base");
    for (file, first, message) in [("f", "A1", "A"), ("g", "B1", "B"), ("f", "X1", "C")] {
        pair.write(file, &F.replacen("l1", first, 1));
        pair.commit_all(message);
    }
    pair.write("g", &F.replacen("l1", "Y1", 1));
    pair.commit_all("D");
    let began = at(&pair);
    let (ours, theirs) = pair.run(&["revert", "HEAD~3", "HEAD~2"]);
    assert_eq!(
        (ours.status.code(), theirs.status.code()),
        (Some(1), Some(1))
    );
    pair.write("f", &F.replacen("l1", "R", 1));
    pair.commit_all("resolved");
    let (ours, theirs) = pair.run(&["revert", "--continue"]);
    assert_eq!(
        (ours.status.code(), theirs.status.code()),
        (Some(1), Some(1))
    );
    let (ours, theirs) = pair.run(&["revert", "--abort"]);
    same(&pair, "abort after a second stop", &ours, &theirs);
    assert_eq!(at(&pair), began);
    assert_eq!(in_progress(&pair), (false, false));
}
