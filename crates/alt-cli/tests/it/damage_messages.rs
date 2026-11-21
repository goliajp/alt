//! A command that meets a damaged record says which record: the errors of
//! the storage layers name only the layer, and what they wrap names the
//! file.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use super::unconfirmed::{alt, ok, stderr};

/// Changes the byte `back` bytes before the end of `path`.
fn damage(path: &Path, back: u64) {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let at = file.metadata().unwrap().len() - back;
    let mut byte = [0u8];
    file.read_exact_at(&mut byte, at).unwrap();
    file.write_all_at(&[!byte[0]], at).unwrap();
}

#[test]
fn a_command_that_meets_a_damaged_record_names_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    ok(alt(dir, &["init"]));
    // large enough to be stored through the blob map
    let large: Vec<u8> = (0..3_000_000u32)
        .map(|n| (n.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    fs::write(dir.join("large.bin"), large).unwrap();
    ok(alt(dir, &["add", "large.bin"]));
    ok(alt(dir, &["commit", "-m", "first"]));

    let log = dir.join(".alt/oplog/log");
    let whole = fs::read(&log).unwrap();
    damage(&log, 40);
    for args in [
        &["status"][..],
        &["rev-parse", "HEAD"],
        &["cat-file", "-p", "HEAD"],
    ] {
        let out = alt(dir, args);
        assert!(!out.status.success(), "{args:?}");
        let said = stderr(&out);
        assert!(said.contains("oplog record corrupt"), "{args:?}: {said}");
    }
    fs::write(&log, whole).unwrap();

    damage(&dir.join(".alt/store/manifests/blobmap"), 20);
    fs::write(dir.join("other.txt"), "other\n").unwrap();
    let out = alt(dir, &["add", "other.txt"]);
    assert!(!out.status.success());
    let said = stderr(&out);
    assert!(said.contains("blobmap record corrupt"), "{said}");
}

/// The last record of a log is damaged and was synced, and the marker
/// that recorded how far the log was synced is empty: nothing tells the
/// record from an append that never finished. No command takes it for one:
/// writes and the full check fail naming the log, and no log is cut.
#[test]
fn a_damaged_record_under_an_empty_marker_is_reported_and_not_cut() {
    const LOGS: [&str; 5] = [
        "store/pack-00000001.altpack",
        "map.alt",
        "oplog/log",
        "odb.durable",
        "oplog/durable",
    ];
    for (log, back, marker, said) in [
        ("oplog/log", 40, "oplog/durable", "oplog record corrupt"),
        ("map.alt", 41, "odb.durable", "map.alt record corrupt"),
    ] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let alt_dir = dir.join(".alt");
        ok(alt(dir, &["init"]));
        fs::write(dir.join("one.txt"), "one\n").unwrap();
        ok(alt(dir, &["add", "one.txt"]));
        ok(alt(dir, &["commit", "-m", "first"]));
        damage(&alt_dir.join(log), back);
        fs::write(alt_dir.join(marker), b"").unwrap();
        let files = || LOGS.map(|name| fs::read(alt_dir.join(name)).unwrap());
        let before = files();

        fs::write(dir.join("other.txt"), "other\n").unwrap();
        for args in [
            &["add", "other.txt"][..],
            &["commit", "-m", "second"],
            &["fsck"],
        ] {
            let out = alt(dir, args);
            assert!(!out.status.success(), "{log}: {args:?}");
            let stderr = stderr(&out);
            assert!(stderr.contains(said), "{log}: {args:?}: {stderr}");
            for (name, (now, was)) in LOGS.iter().zip(files().iter().zip(&before)) {
                assert!(now == was, "{log}: {args:?} changed {name}");
            }
        }
    }
}
