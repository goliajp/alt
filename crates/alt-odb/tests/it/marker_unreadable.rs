//! A marker that does not read — empty, shorter than any marker, or with
//! no slot whose checksum holds — gives no boundary. In a store that has
//! had boundaries they were lost: a record that does not read is reported,
//! naming its log, and nothing is cut. In a store an older alt wrote there
//! never were any, and the last record not reading is taken for an append
//! that never finished, as the older alt takes it.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_git_codec::ObjectKind;
use alt_odb::{NativeOdb, OdbError};

use super::marker::recorded;
use super::rewind_interrupted::{ALL, FILES, Split, lens, object, open, put};

#[derive(Debug, Clone, Copy)]
enum Form {
    Empty,
    /// shorter than the lengths an older alt keeps
    Short,
    /// the magic of this format and no slot whose checksum holds
    NoSlot,
}

const FORMS: [Form; 3] = [Form::Empty, Form::Short, Form::NoSlot];

fn unreadable(dir: &Path, form: Form) {
    let marker = dir.join("odb.durable");
    let mut data = fs::read(&marker).unwrap();
    match form {
        Form::Empty => data.clear(),
        Form::Short => data.truncate(23),
        Form::NoSlot => {
            data[32 + 13] ^= 0xff;
            data[128 + 13] ^= 0xff;
        }
    }
    fs::write(marker, data).unwrap();
}

/// What is damaged in the last record of a log.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Damage {
    /// the encoding byte of the pack's record
    Pack,
    /// a byte of what the pack's record holds
    PackPayload,
    Blobmap,
    Tier1,
    Map,
}

const DAMAGES: [Damage; 5] = [
    Damage::Pack,
    Damage::PackPayload,
    Damage::Blobmap,
    Damage::Tier1,
    Damage::Map,
];

impl Damage {
    /// What a command that meets it says.
    fn said(self) -> &'static str {
        match self {
            Damage::Pack => "active pack damaged",
            Damage::PackPayload => "corrupt",
            Damage::Blobmap => "blobmap record corrupt",
            Damage::Tier1 => "tier1 record corrupt",
            Damage::Map => "map.alt record corrupt",
        }
    }

    fn apply(self, dir: &Path) {
        let (file, back) = match self {
            Damage::Pack | Damage::PackPayload => (0, 0),
            Damage::Blobmap => (1, 40),
            Damage::Tier1 => (2, 36),
            Damage::Map => (3, 41),
        };
        let path = dir.join(FILES[file]);
        let data = fs::read(&path).unwrap();
        let at = match self {
            Damage::Pack => last_pack_record(&data).0 + 32,
            Damage::PackPayload => {
                let (start, len) = last_pack_record(&data);
                start + 41 + len / 2
            }
            _ => data.len() - back,
        };
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.write_all_at(&[!data[at]], at as u64).unwrap();
    }
}

/// Where the last record of a pack starts and how many bytes it holds.
fn last_pack_record(pack: &[u8]) -> (usize, usize) {
    let mut at = 5;
    loop {
        let len = u32::from_le_bytes(pack[at + 37..at + 41].try_into().unwrap()) as usize;
        if at + 41 + len == pack.len() {
            return (at, len);
        }
        at += 41 + len;
    }
}

fn said(error: OdbError) -> String {
    let mut all = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(error) = source {
        all = format!("{all}: {error}");
        source = error.source();
    }
    all
}

fn snapshot(dir: &Path) -> Vec<Vec<u8>> {
    let read = |name: &&str| fs::read(dir.join(name)).unwrap();
    FILES.iter().chain(&["odb.durable"]).map(read).collect()
}

/// What reading every object says first that is not the object.
fn read_all(dir: &Path) -> Option<String> {
    let mut odb = match NativeOdb::open(dir) {
        Ok(odb) => odb,
        Err(e) => return Some(said(e)),
    };
    odb.register_prism(Box::new(Split)).unwrap();
    for n in 0..ALL {
        let (oid, data) = object(n);
        match odb.get(&oid) {
            Ok(Some(read)) => assert_eq!(read.data, data, "object {n}"),
            Ok(None) => return Some(format!("object {n} is not there")),
            Err(e) => return Some(said(e)),
        }
    }
    None
}

/// What storing one more object says, when it fails.
fn write_one(dir: &Path) -> Option<String> {
    let mut odb = match NativeOdb::open(dir) {
        Ok(odb) => odb,
        Err(e) => return Some(said(e)),
    };
    let (oid, data) = object(ALL);
    let stored = odb
        .put(oid, ObjectKind::Blob, &data)
        .and_then(|_| odb.flush());
    stored.err().map(said)
}

#[test]
fn a_damaged_last_record_is_reported_by_its_log_and_never_cut() {
    for form in FORMS {
        for damage in DAMAGES {
            let case = format!("{form:?}, {damage:?}");
            let dir = alt_os::temp::tempdir().unwrap();
            let mut odb = open(dir.path());
            put(&mut odb, 0..ALL);
            drop(odb);
            damage.apply(dir.path());
            unreadable(dir.path(), form);
            let before = snapshot(dir.path());

            let read = read_all(dir.path()).unwrap_or_default();
            assert!(read.contains(damage.said()), "{case}: a read said {read:?}");
            assert!(snapshot(dir.path()) == before, "{case}: a read cut");

            let written = write_one(dir.path());
            let after = snapshot(dir.path());
            if damage == Damage::PackPayload {
                // the record is whole in structure: what it holds is
                // looked at when it is read
                assert_eq!(written, None, "{case}");
                let grew = (0..4).all(|n| after[n].starts_with(&before[n]));
                assert!(grew, "{case}: a write cut");
                let read = read_all(dir.path()).unwrap_or_default();
                assert!(read.contains(damage.said()), "{case}: then {read:?}");
            } else {
                let written = written.unwrap_or_default();
                assert!(
                    written.contains(damage.said()),
                    "{case}: a write said {written:?}"
                );
                assert!(after == before, "{case}: a write changed a file");
            }
        }
    }
}

#[test]
fn a_store_that_never_had_boundaries_takes_its_damaged_last_record_for_unfinished() {
    for form in FORMS {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = open(dir.path());
        put(&mut odb, 0..ALL);
        drop(odb);
        let whole = lens(dir.path());
        Damage::Map.apply(dir.path());
        unreadable(dir.path(), form);
        // as an older alt leaves it: no head record
        fs::write(dir.path().join("odb.lock"), b"").unwrap();

        assert_eq!(write_one(dir.path()), None, "{form:?}");
        let after = lens(dir.path());
        assert_eq!(after[3], whole[3], "{form:?}: one record cut, one written");
        let marker = recorded(dir.path());
        assert_eq!((marker.flags, marker.whole), (0, after), "{form:?}");
        let odb = open(dir.path());
        let (last, _) = object(ALL - 1);
        assert!(odb.get(&last).unwrap().is_none(), "{form:?}");
        let (new, data) = object(ALL);
        assert_eq!(odb.get(&new).unwrap().unwrap().data, data, "{form:?}");
    }
}
