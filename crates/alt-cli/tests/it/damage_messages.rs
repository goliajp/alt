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
