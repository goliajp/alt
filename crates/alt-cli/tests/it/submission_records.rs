//! The record that a request was submitted is there for whoever holds the
//! request's id before the request is claimed: a caller that chose the
//! id, and a client that hands the command to a daemon. An id drawn by
//! the process that goes on to run the command itself is known to nobody
//! else, and that process records nothing before it claims the request.

use std::fs;
use std::path::Path;

use super::scan_daemon::{Stop, daemon_pid, run, write};

/// The ids of the requests recorded in `directory` of the control directory.
fn recorded(root: &Path, directory: &str) -> Vec<String> {
    let Ok(listing) = fs::read_dir(root.join(".alt").join(directory)) else {
        return Vec::new();
    };
    let name =
        |entry: std::io::Result<fs::DirEntry>| entry.unwrap().file_name().into_string().unwrap();
    let mut ids: Vec<String> = listing.map(name).collect();
    ids.sort();
    ids
}

#[test]
fn a_command_run_here_under_an_id_it_drew_records_the_request_and_no_submission() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path();
    run(root, &["init"], false);
    write(root, "a.txt", "a\n");
    run(root, &["add", "a.txt"], false);
    run(root, &["commit", "-m", "first"], false);
    assert!(!recorded(root, "requests").is_empty());
    assert_eq!(recorded(root, "submissions"), [""; 0]);
    // an id the caller chose is recorded as submitted before it is claimed
    let id = "000102030405060708090a0b0c0d0e0f";
    write(root, "b.txt", "b\n");
    run(root, &["--request-id", id, "add", "b.txt"], false);
    assert_eq!(recorded(root, "submissions"), [format!("{id}.json")]);
    assert!(recorded(root, "requests").contains(&id.to_owned()));
    let status = run(root, &["request", "status", id], false);
    assert!(status.contains("completed"), "{status}");
}

#[test]
fn a_command_handed_to_the_daemon_is_recorded_as_submitted_by_the_client() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path();
    run(root, &["init"], false);
    write(root, "a.txt", "a\n");
    run(root, &["add", "a.txt"], true);
    let _stop = Stop(daemon_pid(root), false);
    let submitted = recorded(root, "submissions");
    assert_eq!(submitted.len(), 1);
    let id = submitted[0].strip_suffix(".json").unwrap();
    assert!(recorded(root, "requests").contains(&id.to_owned()));
}
