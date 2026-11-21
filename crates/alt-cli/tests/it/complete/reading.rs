//! Answering only reads, whoever answers.

use std::os::unix::net::UnixStream;
use std::path::Path;

use super::{alt, answer, asked, repository};
use crate::unconfirmed::{Lost, power_cut};

/// Every file under `dir` with its length and the time it was last
/// written.
fn files(dir: &Path, found: &mut Vec<(std::path::PathBuf, u64, std::time::SystemTime)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let meta = entry.metadata().unwrap();
        if meta.is_dir() {
            files(&entry.path(), found);
        } else {
            found.push((entry.path(), meta.len(), meta.modified().unwrap()));
        }
    }
}

#[test]
fn answering_changes_no_file_of_the_repository() {
    let dir = repository();
    let root = dir.path();
    // what any command that reads brings up to date
    alt(root, &["status"]);
    let mut before = Vec::new();
    files(&root.join(".alt"), &mut before);
    assert!(before.len() > 3, "{before:?}");
    for line in [
        "alt switch ",
        "alt show ",
        "alt add ",
        "alt blame ",
        "alt push origin ",
        "alt config ",
        "alt --workspace ",
    ] {
        assert_ne!(answer(root, line), "", "{line}");
    }
    let mut after = Vec::new();
    files(&root.join(".alt"), &mut after);
    assert_eq!(before, after);
}

#[test]
fn the_daemon_answers_as_the_command_itself_does() {
    let dir = repository();
    let root = dir.path();
    for line in ["alt switch ", "alt add ", "alt show ", "alt commit -"] {
        assert_eq!(asked(root, line, true), answer(root, line), "{line}");
    }
    let sock = root.join(".alt").join("daemon.sock");
    assert!(UnixStream::connect(&sock).is_ok(), "no daemon came up");
}

#[test]
fn operations_that_were_never_synced_are_passed_over_without_a_word() {
    let dir = alt_os::temp::tempdir().unwrap();
    power_cut(dir.path(), Lost::NotesCommit);
    // `answer` holds every answer to an empty stderr
    assert_eq!(
        answer(dir.path(), "alt switch "),
        "group\tbranches\tbranch\nmain\t\nwithout\t\n"
    );
}
