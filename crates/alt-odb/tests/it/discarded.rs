//! A record that does not read past the synced boundary of a file, with
//! whole records after it: a reader says beforehand what a writer cuts
//! off the four files and which objects go with it, and the writer keeps
//! what it cut for whoever tells the user.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_odb::{Discarded, NativeOdb};

use super::dangling::synced_to;
use super::rewind_interrupted::{ALL, FILES, KEPT, Split, lens, object, open, put};

fn flip(path: &Path, at: u64) {
    let log = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut byte = [0u8];
    log.read_exact_at(&mut byte, at).unwrap();
    log.write_all_at(&[!byte[0]], at).unwrap();
}

/// How many bytes `discarded` says went off each of the four files.
fn cut_off(dir: &Path, discarded: &[Discarded]) -> Vec<u64> {
    let of = |file: &&str| {
        let cuts = discarded.iter().filter(|cut| cut.file == dir.join(file));
        cuts.map(|cut| cut.to - cut.from).sum()
    };
    FILES.iter().map(of).collect()
}

#[test]
fn a_reader_tells_what_a_writer_then_cuts_and_keeps_word_of() {
    for (file, name) in FILES.iter().enumerate() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let mut odb = open(dir);
        put(&mut odb, 0..KEPT);
        let kept = lens(dir);
        put(&mut odb, KEPT..ALL);
        drop(odb);
        let full = lens(dir);
        synced_to(dir, kept, 0);
        // the first record past the boundary: a byte of what a pack
        // record holds, of the key of any other
        flip(&dir.join(name), kept[file] + [41, 10, 10, 10][file]);

        let mut reader = NativeOdb::read_only(dir);
        reader.register_prism(Box::new(Split)).unwrap();
        assert_eq!(reader.unsynced_from().unwrap(), kept.map(Some), "{name}");
        let passed = reader.passed().unwrap();
        assert_eq!(passed[file].1.holds, kept[file], "{name}");
        assert!(passed[file].1.whole_at.is_some(), "{name}");
        let named = reader.check().unwrap().unconfirmed;
        let named = [0, named[0], named[1], named[2]];
        let told: Vec<u64> = (0..4)
            .map(|n| passed[n].1.len - passed[n].1.holds + named[n])
            .collect();
        let gone = reader.unconfirmed_objects().unwrap();
        // what `map.alt` holds past the record that does not read is not
        // there for a reader in the first place
        let map = file == 3;
        assert_eq!(gone.is_empty(), map, "{name}");
        drop(reader);
        assert_eq!(lens(dir), full, "{name}: a reader changed a file");

        let mut odb = open(dir);
        put(&mut odb, ALL + 3..ALL + 4);
        let cut = cut_off(dir, &odb.take_discarded());
        assert_eq!(cut, told, "{name}");
        assert!(cut[file] >= full[file] - kept[file], "{name}");
        assert!(odb.take_discarded().is_empty(), "{name}");
        for n in KEPT..ALL {
            let (oid, _) = object(n);
            let stored = odb.contains(&oid).unwrap();
            assert_eq!(stored, !map && !gone.contains(&oid), "{name}: object {n}");
        }
        for n in 0..KEPT {
            assert!(odb.contains(&object(n).0).unwrap(), "{name}: object {n}");
        }
    }
}

#[test]
fn a_store_whose_files_read_to_their_ends_has_nothing_cut() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let mut odb = open(dir);
    put(&mut odb, 0..KEPT);
    let kept = lens(dir);
    put(&mut odb, KEPT..ALL);
    drop(odb);
    // records past the boundary that read and name content that is stored
    synced_to(dir, kept, 0);
    let reader = NativeOdb::read_only(dir);
    assert!(reader.unconfirmed_objects().unwrap().is_empty());
    drop(reader);
    let full = lens(dir);
    let mut odb = open(dir);
    put(&mut odb, ALL + 3..ALL + 4);
    assert!(odb.take_discarded().is_empty());
    assert!(lens(dir).iter().zip(full).all(|(now, was)| *now >= was));
}
