//! A rewind that a crash interrupts at any of its steps: the marker comes
//! down, then `map.alt`, the Tier 1 map, the blob map and the pack are cut
//! in that order, then the marker is written again. Whatever step it
//! stopped after, the store opens, what was to stay reads, and nothing is
//! said to be there that does not read.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_prism::{Decomposition, Prism, PrismId};

use super::marker::rewrite;

const FILES: [&str; 4] = [
    "store/pack-00000001.altpack",
    "store/manifests/blobmap",
    "tier1",
    "map.alt",
];
const KEPT: u32 = 30;
const ALL: u32 = 60;

/// Splits what starts with `split:` in two, so that it is stored through
/// the Tier 1 map.
struct Split;

impl Prism for Split {
    fn id(&self) -> PrismId {
        PrismId(0x7e)
    }

    fn decompose(&self, input: &[u8]) -> Option<Decomposition> {
        let mid = input.len() / 2;
        input.starts_with(b"split:").then(|| Decomposition {
            recipe: (mid as u32).to_le_bytes().to_vec(),
            parts: vec![input[..mid].to_vec(), input[mid..].to_vec()],
        })
    }

    fn recompose(&self, recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
        let mid = u32::from_le_bytes(recipe.try_into().ok()?) as usize;
        let [head, tail] = parts else { return None };
        (head.len() == mid).then(|| [*head, *tail].concat())
    }
}

/// Every third object is one chunk, every third several chunks under a
/// blob map record, every third goes through the Tier 1 map.
fn object(n: u32) -> (ObjectId, Vec<u8>) {
    let data = match n % 3 {
        0 => format!("object number {n} {}", "x".repeat(60)).into_bytes(),
        1 => {
            let mut out = Vec::new();
            let mut word = (u64::from(n) + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            // past the largest chunk there is
            while out.len() < 300_000 {
                word ^= word << 13;
                word ^= word >> 7;
                word ^= word << 17;
                out.extend_from_slice(&word.to_le_bytes());
            }
            out
        }
        _ => format!("split: object number {n} {}", "y".repeat(90)).into_bytes(),
    };
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

fn open(dir: &Path) -> NativeOdb {
    let mut odb = NativeOdb::open(dir).unwrap();
    odb.register_prism(Box::new(Split)).unwrap();
    odb
}

fn lens(dir: &Path) -> [u64; 4] {
    FILES.map(|name| fs::metadata(dir.join(name)).unwrap().len())
}

fn put(odb: &mut NativeOdb, objects: std::ops::Range<u32>) {
    for n in objects {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
}

fn cut(dir: &Path, file: usize, len: u64) {
    let file = OpenOptions::new()
        .write(true)
        .open(dir.join(FILES[file]))
        .unwrap();
    file.set_len(len).unwrap();
}

/// A store of `ALL` objects with the marker as a rewind to the first
/// `KEPT` brings it down before it cuts anything. Returns the lengths the
/// rewind cuts the files to.
fn rewind_begun(dir: &Path) -> [u64; 4] {
    let mut odb = open(dir);
    put(&mut odb, 0..KEPT);
    let kept = lens(dir);
    put(&mut odb, KEPT..ALL);
    drop(odb);
    let full = lens(dir);
    assert!((0..4).all(|n| kept[n] < full[n]), "{kept:?} {full:?}");
    rewrite(dir, |marker| {
        marker.generation += 1;
        marker.eof = kept;
        marker.whole = kept;
    });
    kept
}

fn reads(odb: &NativeOdb, n: u32) {
    let (oid, data) = object(n);
    let read = odb.get(&oid).unwrap().expect("object missing");
    assert_eq!(read.data, data, "object {n}");
}

/// The store opens, holds what the rewind keeps, and takes what it rolled
/// back again.
fn recovers(dir: &Path, step: &str) {
    let mut odb = open(dir);
    for n in 0..KEPT {
        reads(&odb, n);
    }
    for n in KEPT..ALL {
        // rolled back, or still whole: never named and not readable
        if odb.contains(&object(n).0).unwrap() {
            reads(&odb, n);
        }
    }
    put(&mut odb, KEPT..ALL);
    for n in 0..ALL {
        reads(&odb, n);
    }
    drop(odb);
    let odb = open(dir);
    for n in 0..ALL {
        reads(&odb, n);
    }
    odb.check().unwrap_or_else(|e| panic!("{step}: {e:?}"));
}

#[test]
fn a_rewind_interrupted_after_any_step_leaves_no_record_of_content_that_is_gone() {
    // the order the rewind cuts in: map.alt, tier1, blob map, pack
    let order = [3, 2, 1, 0];
    for cuts in 0..=order.len() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let kept = rewind_begun(tmp.path());
        for &file in &order[..cuts] {
            cut(tmp.path(), file, kept[file]);
        }
        let step = match cuts {
            0 => "after the marker came down".to_owned(),
            n => format!("after {} was cut", FILES[order[n - 1]]),
        };
        recovers(tmp.path(), &step);
    }
}

#[test]
fn a_rewind_that_ran_to_its_end_but_for_the_last_marker_write_is_the_same_as_finished() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let kept = rewind_begun(tmp.path());
    for file in [3, 2, 1, 0] {
        cut(tmp.path(), file, kept[file]);
    }
    let odb = open(tmp.path());
    for n in KEPT..ALL {
        assert!(!odb.contains(&object(n).0).unwrap(), "object {n}");
    }
    drop(odb);
    recovers(tmp.path(), "before the marker is written again");
    assert_eq!(lens(tmp.path())[3], {
        let other = alt_os::temp::tempdir().unwrap();
        put(&mut open(other.path()), 0..ALL);
        lens(other.path())[3]
    });
}

/// Watches the files while a rewind cuts them: at no moment is a file cut
/// while one that names its content is not.
#[test]
fn the_rewind_cuts_what_names_content_before_the_content() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let mut odb = open(tmp.path());
    put(&mut odb, 0..KEPT);
    let kept = lens(tmp.path());
    let ckpt = odb.checkpoint().unwrap();
    put(&mut odb, KEPT..ALL);
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let watch = scope.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                let now = lens(tmp.path());
                // pack, blob map, tier1, map.alt: each is named by the next
                for n in 0..3 {
                    assert!(now[n] > kept[n] || now[n + 1] == kept[n + 1], "{now:?}");
                }
            }
        });
        odb.rewind(ckpt).unwrap();
        done.store(true, Ordering::Relaxed);
        watch.join().unwrap();
    });
    assert_eq!(lens(tmp.path()), kept);
    drop(odb);
    recovers(tmp.path(), "after a whole rewind");
}

/// What an older alt leaves when its rewind, which cut the pack first, is
/// interrupted: records that name content the pack no longer holds. Such
/// an object is reported as not readable, never read as other bytes.
#[test]
fn records_an_older_rewind_left_naming_cut_content_never_read_as_other_bytes() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let kept = rewind_begun(tmp.path());
    cut(tmp.path(), 0, kept[0]);
    let mut odb = open(tmp.path());
    for n in 0..KEPT {
        reads(&odb, n);
    }
    for n in KEPT..ALL {
        let (oid, data) = object(n);
        assert!(odb.contains(&oid).unwrap());
        match odb.get(&oid) {
            Ok(Some(read)) => assert_eq!(read.data, data, "object {n}"),
            Ok(None) => panic!("object {n} is named and reads as absent"),
            Err(_) => {}
        }
    }
    assert!(odb.check().is_err());
    // writing such an object again does not bring it back: the records
    // that name it are taken at their word
    for n in KEPT..ALL {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
    for n in KEPT..ALL {
        let (oid, data) = object(n);
        if let Ok(Some(read)) = odb.get(&oid) {
            assert_eq!(read.data, data, "object {n}");
        }
    }
    assert!(odb.check().is_err());
}
