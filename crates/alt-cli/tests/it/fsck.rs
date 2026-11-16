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

const INDEX_DIRS: [&str; 4] = [
    ".alt/map.alt.idx",
    ".alt/tier1.idx",
    ".alt/store/chunks.idx",
    ".alt/store/manifests/blobmap.idx",
];

fn runs_in(dir: &Path) -> Vec<std::path::PathBuf> {
    let files = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path());
    files
        .filter(|path| path.extension().is_some_and(|e| e == "run"))
        .collect()
}

#[test]
fn an_index_run_no_open_can_use_is_named() {
    // cut short, emptied, and with a damaged header
    let damages: [fn(&Path); 3] = [
        |run| {
            fs::OpenOptions::new()
                .write(true)
                .open(run)
                .unwrap()
                .set_len(40)
                .unwrap()
        },
        |run| fs::write(run, b"").unwrap(),
        |run| {
            let file = fs::OpenOptions::new().write(true).open(run).unwrap();
            file.write_all_at(&[0xff], 10).unwrap()
        },
    ];
    for damage in damages {
        let tmp = alt_os::temp::tempdir().unwrap();
        setup(tmp.path());
        let run = runs_in(&tmp.path().join(".alt/map.alt.idx")).pop().unwrap();
        damage(&run);
        let out = ok(alt(tmp.path(), &["fsck"]));
        let named = format!("ignored index run {}: not a complete run\n", run.display());
        assert!(out.contains(&named), "got: {out}");
        assert!(out.ends_with("every record verified\n"), "got: {out}");
        // that open put a whole run in its place
        assert!(!ok(alt(tmp.path(), &["fsck"])).contains("ignored"));
        let run = runs_in(&tmp.path().join(".alt/map.alt.idx")).pop().unwrap();
        damage(&run);
        let json = ok(alt(tmp.path(), &["fsck", "--json"]));
        let listed = format!("\"ignored_index_runs\":[\"{}\"]", run.display());
        assert!(json.contains(&listed), "got: {json}");
    }
}

#[test]
fn removing_the_index_directories_repairs_them_and_loses_nothing() {
    let tmp = alt_os::temp::tempdir().unwrap();
    setup(tmp.path());
    let remove_indexes = || {
        for dir in INDEX_DIRS {
            match fs::remove_dir_all(tmp.path().join(dir)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => panic!("{dir}: {e}"),
                _ => {}
            }
        }
    };
    remove_indexes();
    let log = ok(alt(tmp.path(), &["log"]));
    assert_eq!(log.lines().count(), 2, "got: {log}");
    let out = ok(alt(tmp.path(), &["fsck"]));
    assert!(!out.contains("ignored"), "got: {out}");
    assert!(out.ends_with("every record verified\n"), "got: {out}");
    assert_eq!(runs_in(&tmp.path().join(".alt/map.alt.idx")).len(), 1);

    // over a damaged record the rebuild reports it and cuts nothing off
    let map = tmp.path().join(".alt/map.alt");
    flip(&map, 5 + 100 * 82 + 40);
    remove_indexes();
    let before = fs::metadata(&map).unwrap().len();
    for command in [&["cat-file", "-p", "HEAD"][..], &["fsck"]] {
        let out = alt(tmp.path(), command);
        assert!(!out.status.success(), "alt {command:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("map.alt record corrupt"), "got: {stderr}");
    }
    assert_eq!(fs::metadata(&map).unwrap().len(), before);
}
