//! `alt fsck` reads every record; the commands before it read only what
//! they need and so pass over damage elsewhere.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// Two commits over 300 files: enough records for the stores to index.
fn setup(repo: &Path) {
    ok(alt(repo, &["init"]));
    for n in 0..300 {
        fs::write(repo.join(format!("f{n}.txt")), format!("file {n}\n")).unwrap();
    }
    ok(alt(repo, &["add", "."]));
    ok(alt(repo, &["commit", "-m", "three hundred files"]));
    fs::write(repo.join("f0.txt"), "changed\n").unwrap();
    ok(alt(repo, &["add", "f0.txt"]));
    ok(alt(repo, &["commit", "-m", "one change"]));
}

fn flip(path: &Path, at: u64) {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut byte = [0u8; 1];
    file.read_exact_at(&mut byte, at).unwrap();
    byte[0] ^= 0x20;
    file.write_all_at(&byte, at).unwrap();
}

#[test]
fn a_sound_repository_passes_and_reports_what_it_read() {
    let tmp = alt_os::temp::tempdir().unwrap();
    setup(tmp.path());
    let out = ok(alt(tmp.path(), &["fsck"]));
    assert!(out.ends_with("every record verified\n"), "got: {out}");
    let objects: u64 = out
        .lines()
        .find_map(|line| line.trim().strip_suffix(" objects"))
        .expect("an objects line")
        .parse()
        .unwrap();
    // 301 file contents, two trees and two commits, plus commit notes
    assert!(objects >= 305, "got: {out}");
    let json = ok(alt(tmp.path(), &["fsck", "--json"]));
    assert!(json.starts_with("{\"schema_version\":1,"), "got: {json}");
    assert!(
        json.contains(&format!("\"objects\":{objects},")),
        "got: {json}"
    );
}

#[test]
fn damage_in_a_record_no_command_reads_fails_fsck_only() {
    let tmp = alt_os::temp::tempdir().unwrap();
    setup(tmp.path());
    let map = tmp.path().join(".alt/map.alt");
    assert!(
        fs::read_dir(tmp.path().join(".alt/map.alt.idx"))
            .unwrap()
            .next()
            .is_some(),
        "the object map has index runs"
    );
    // record 100 of the object map (5-byte header, 82-byte records): the
    // identity of one file's content, which no command below looks up
    flip(&map, 5 + 100 * 82 + 40);

    ok(alt(tmp.path(), &["status"]));
    let log = ok(alt(tmp.path(), &["log"]));
    assert_eq!(log.lines().count(), 2, "got: {log}");

    let out = alt(tmp.path(), &["fsck"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("map.alt record corrupt"), "got: {stderr}");
}
