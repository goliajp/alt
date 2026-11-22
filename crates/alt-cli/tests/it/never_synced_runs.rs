//! A process that never syncs (`ALT_RELAXED_DURABILITY`) folds what it
//! appends into index runs of the chain as it goes. Handed a boot run of
//! the chunk index, which an add before it left, it builds the run of
//! the chain over the records of that run too.

use std::fs;
use std::path::Path;

use super::staged_durability::repo;
use super::sync_order::{lens, objects_synced};
use super::unconfirmed::{alt, alt_command, ok};

/// Where the runs of the chain of the chunk index end in the pack, and
/// how many boot run files lie beside them.
fn chunk_runs(alt_dir: &Path) -> (Vec<u64>, usize) {
    let (mut ends, mut boot) = (Vec::new(), 0);
    for entry in fs::read_dir(alt_dir.join("store/chunks.idx")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            boot += fs::read_dir(entry.path()).unwrap().count();
            continue;
        }
        let name = entry.file_name().into_string().unwrap();
        if let Some((_, end)) = name.strip_suffix(".run").and_then(|n| n.split_once('-')) {
            // the pack is named above forty bits
            ends.push(u64::from_str_radix(end, 16).unwrap() & ((1 << 40) - 1));
        }
    }
    (ends, boot)
}

#[test]
fn a_process_that_never_syncs_builds_a_run_of_the_chain_over_a_boot_run_of_the_chunk_index() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let alt_dir = dir.join(".alt");
    let synced = objects_synced(&alt_dir)[0];
    let files = |from: u32| {
        for n in from..from + 300 {
            let text = format!("file number {n}\n").repeat(40);
            fs::write(dir.join(format!("f{n}.txt")), text).unwrap();
        }
    };
    files(0);
    ok(alt(dir, &["add", "."]));
    let (ends, boot) = chunk_runs(&alt_dir);
    assert_eq!(boot, 1);
    assert!(ends.iter().all(|end| *end <= synced), "{ends:?}");
    files(300);
    let mut never_synced = alt_command(dir, &["add", "."]);
    ok(never_synced
        .env("ALT_RELAXED_DURABILITY", "1")
        .output()
        .unwrap());
    let (ends, boot) = chunk_runs(&alt_dir);
    assert_eq!(boot, 0);
    assert!(ends.contains(&lens(&alt_dir)[0]), "{ends:?}");
    ok(alt(dir, &["commit", "-m", "second"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with("every record verified\n"));
    let status = ok(alt(dir, &["status"]));
    assert!(status.contains("nothing to commit"), "{status}");
}
