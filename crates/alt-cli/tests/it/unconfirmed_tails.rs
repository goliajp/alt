//! A record that does not read past the boundary a file was last recorded
//! as synced to, with whole records after it, and whole records and
//! operations there that name content which is not stored: nothing there
//! was ever confirmed. `alt fsck` says what goes with the next command
//! that writes, or with the next that writes objects, and exits 3; that
//! command says what it discarded, and the two agree with what left the
//! files. The same record before the boundary, or without a boundary that
//! holds, is damage: reported, and nothing is cut.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::Output;

use super::unconfirmed::{Lost, alt, alt_command, files, ok, power_cut, stderr};

pub(super) const LOGS: [&str; 5] = [
    "store/pack-00000001.altpack",
    "store/manifests/blobmap",
    "tier1",
    "map.alt",
    "oplog/log",
];
pub(super) const MARKERS: [&str; 2] = ["odb.durable", "oplog/durable"];

/// Bytes that neither compress nor repeat, `seed` telling one run from
/// another.
pub(super) fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut word = 0x9e37_79b9_7f4a_7c15u64 ^ seed;
    (0..len)
        .map(|_| {
            word ^= word << 13;
            word ^= word >> 7;
            word ^= word << 17;
            word as u8
        })
        .collect()
}

/// An archive of one stored entry, which the store keeps as parts.
pub(super) fn archive(body: &[u8]) -> Vec<u8> {
    let (name, size) = (b"part.bin", body.len() as u32);
    let mut out = b"PK\x03\x04".to_vec();
    out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(body);
    let directory = out.len() as u32;
    out.extend_from_slice(b"PK\x01\x02");
    out.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    out.extend_from_slice(name);
    let directory_len = out.len() as u32 - directory;
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
    out.extend_from_slice(&directory_len.to_le_bytes());
    out.extend_from_slice(&directory.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

pub(super) fn lens(dir: &Path) -> [u64; 5] {
    LOGS.map(|log| fs::metadata(dir.join(".alt").join(log)).unwrap().len())
}

/// One commit, then two more that add records to all five logs. Returns
/// how long the logs were after the first, with the two markers as they
/// were then.
fn repo(dir: &Path) -> ([u64; 5], [Vec<u8>; 2]) {
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    let first = lens(dir);
    let markers = MARKERS.map(|marker| fs::read(dir.join(".alt").join(marker)).unwrap());
    for round in 1..=2u64 {
        let names = [
            format!("t{round}.txt"),
            format!("l{round}.bin"),
            format!("z{round}.zip"),
        ];
        fs::write(dir.join(&names[0]), format!("text {round}\n")).unwrap();
        fs::write(dir.join(&names[1]), noise(round, 1 << 20)).unwrap();
        fs::write(dir.join(&names[2]), archive(&noise(round + 8, 4000))).unwrap();
        for name in &names {
            ok(alt(dir, &["add", name]));
        }
        ok(alt(dir, &["commit", "-m", &format!("round {round}")]));
    }
    let last = lens(dir);
    for (log, (first, last)) in LOGS.iter().zip(first.iter().zip(last)) {
        assert!(last > *first, "{log} holds nothing of the later commits");
    }
    (first, markers)
}

/// Puts the markers back to what they recorded after the first commit.
pub(super) fn lag(dir: &Path, markers: &[Vec<u8>; 2]) {
    for (marker, saved) in MARKERS.iter().zip(markers) {
        fs::write(dir.join(".alt").join(marker), saved).unwrap();
    }
    // the lock file's note of how far the pack was synced
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(dir.join(".alt/odb.lock"))
        .unwrap();
    lock.write_all_at(&[0u8; 24], 1176).unwrap();
}

/// Changes a byte of the record that begins at `at` of log `n`.
pub(super) fn damage(dir: &Path, n: usize, at: u64) {
    // past a pack record's header, into what it holds
    let within = [41, 10, 10, 10, 14][n];
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(".alt").join(LOGS[n]))
        .unwrap();
    let mut byte = [0u8];
    file.read_exact_at(&mut byte, at + within).unwrap();
    file.write_all_at(&[!byte[0]], at + within).unwrap();
}

/// What is said to go: bytes off each file, operations, and what they did
/// to the refs.
type Told = (BTreeMap<String, u64>, usize, Vec<String>);

/// What goes with any command that writes, and with one that writes
/// objects, as the check heads them.
const ANY: &str = "the next command that writes discards:\n";
const OBJECTS: &str = "the next command that writes objects discards:\n";

/// What the check `said` goes with the command of `header`.
fn told_by(said: &str, header: &str) -> Told {
    let mut told: Told = Default::default();
    let block = said.split(header).nth(1);
    for line in block.unwrap_or("").lines() {
        let line = line.trim_start();
        if let Some((bytes, file)) = line.split_once(" byte(s) at the end of ") {
            told.0.insert(file.to_owned(), bytes.parse().unwrap());
        } else if let Some(ops) = line.strip_suffix(" operation(s) at the end of the operation log")
        {
            told.1 = ops.parse().unwrap();
        } else if line.starts_with("to keep them") || line.starts_with("the next command") {
            break;
        } else {
            told.2.push(line.to_owned());
        }
    }
    told
}

/// Runs `alt fsck`, which is to change no file; gives its exit code, what
/// it printed, and what it said the next command that writes discards.
fn fsck(dir: &Path) -> (Option<i32>, String, Told) {
    let before = files(&dir.join(".alt"));
    let out = alt(dir, &["fsck"]);
    assert!(files(&dir.join(".alt")) == before, "fsck changed a file");
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    let told = told_by(&said, ANY);
    (out.status.code(), said + &stderr(&out), told)
}

/// What a command that wrote said on stderr it discarded.
fn warned(out: &Output) -> Told {
    let mut told: Told = Default::default();
    for line in stderr(out).lines() {
        let Some(line) = line.strip_prefix("warning: ") else {
            continue;
        };
        if let Some(line) = line.strip_prefix("  ") {
            told.2.push(line.to_owned());
        } else if let Some((bytes, rest)) = line
            .strip_prefix("discarded ")
            .and_then(|line| line.split_once(" byte(s) at the end of "))
        {
            let file = rest.split(": ").next().unwrap();
            told.0.insert(file.to_owned(), bytes.parse().unwrap());
        } else if let Some((ops, _)) = line
            .strip_prefix("discarded ")
            .and_then(|line| line.split_once(" operation(s) at the end of the operation log"))
        {
            told.1 = ops.parse().unwrap();
        }
    }
    told
}

/// Where the last record of the operation log `bytes` begins.
fn last_op(bytes: &[u8]) -> u64 {
    let mut at = 5;
    loop {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        if at + 4 + len + 8 == bytes.len() {
            break at as u64;
        }
        at += 4 + len + 8;
    }
}

/// The bytes that left the end of each log since they were `before` long,
/// a command having appended one operation since and nothing else.
fn went(dir: &Path, before: [u64; 5]) -> BTreeMap<String, u64> {
    let after = lens(dir);
    let ops = fs::read(dir.join(".alt/oplog/log")).unwrap();
    let ends = [after[0], after[1], after[2], after[3], last_op(&ops)];
    let cut = |m: usize| (LOGS[m].to_owned(), before[m] - ends[m]);
    (0..5).map(cut).filter(|(_, cut)| *cut > 0).collect()
}

#[test]
fn what_the_check_says_goes_is_what_the_next_writer_says_it_cut_and_what_it_cut() {
    for (n, log) in LOGS.iter().enumerate() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let (first, markers) = repo(dir);
        lag(dir, &markers);
        damage(dir, n, first[n]);
        let before = lens(dir);

        let (code, said, told) = fsck(dir);
        assert_eq!(code, Some(3), "{log}: {said}");
        let tail = format!(
            "unconfirmed tail: {log} reads up to byte {}; the {} byte(s) after it were never \
             confirmed as synced, the record at byte {} does not read, and a whole record begins",
            first[n],
            before[n] - first[n],
            first[n]
        );
        assert!(said.contains(&tail), "{log}: {said}");
        assert!(said.contains("fatal: fsck: unconfirmed tail: "), "{said}");
        assert!(said.contains("copy the .alt directory"), "{log}: {said}");
        assert!(told.0[*log] >= before[n] - first[n], "{log}: {said}");
        assert!(told.1 > 0 && !told.2.is_empty(), "{log}: {said}");
        let json = alt(dir, &["fsck", "--json"]);
        assert_eq!(json.status.code(), Some(3), "{log}");
        assert!(stderr(&json).contains("\"kind\":\"unconfirmed_tail\""));

        // a command that writes a ref and nothing else: the files end
        // where it cut them, the log one operation after
        let wrote = alt(dir, &["tag", "after"]);
        assert!(wrote.status.success(), "{log}: {}", stderr(&wrote));
        assert_eq!(warned(&wrote), told, "{log}: {}", stderr(&wrote));
        assert_eq!(went(dir, before), told.0, "{log}");

        let (code, said, told) = fsck(dir);
        assert_eq!((code, told), (Some(0), Told::default()), "{log}: {said}");
        assert!(said.ends_with("every record verified\n"), "{log}: {said}");
        assert!(!said.contains("never"), "{log}: {said}");
    }
}

#[test]
fn the_same_record_before_the_boundary_is_damage_and_nothing_is_cut() {
    for (n, log) in LOGS.iter().enumerate() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let (first, _) = repo(dir);
        // the markers record everything as synced
        damage(dir, n, first[n]);
        let before = LOGS.map(|log| fs::read(dir.join(".alt").join(log)).unwrap());

        let (code, said, told) = fsck(dir);
        assert_eq!(code, Some(128), "{log}: {said}");
        assert!(!said.contains("unconfirmed tail"), "{log}: {said}");
        assert_eq!(told, Told::default(), "{log}");
        fs::write(dir.join("new.txt"), "new\n").unwrap();
        let wrote = alt(dir, &["add", "new.txt"]);
        assert!(!stderr(&wrote).contains("discarded"), "{log}: {wrote:?}");
        for (file, before) in LOGS.iter().zip(&before) {
            let now = fs::read(dir.join(".alt").join(file)).unwrap();
            assert!(now.starts_with(before), "{log}: {file} was cut");
        }
        assert_eq!(fsck(dir).0, Some(128), "{log}");
    }
}

fn relaxed(dir: &Path, args: &[&str]) -> Output {
    let mut command = alt_command(dir, args);
    command.env("ALT_RELAXED_DURABILITY", "1").output().unwrap()
}

#[test]
fn without_a_boundary_that_holds_the_same_record_is_damage_and_nothing_is_cut() {
    // the markers gone, and the markers of a store whose last writes were
    // not synced
    for gone in [true, false] {
        for (n, log) in LOGS.iter().enumerate() {
            let tmp = alt_os::temp::tempdir().unwrap();
            let dir = tmp.path();
            let (first, _) = repo(dir);
            if gone {
                for marker in MARKERS {
                    fs::remove_file(dir.join(".alt").join(marker)).unwrap();
                }
            } else {
                fs::write(dir.join("r.txt"), "relaxed\n").unwrap();
                ok(relaxed(dir, &["add", "r.txt"]));
                ok(relaxed(dir, &["commit", "-m", "not synced"]));
            }
            damage(dir, n, first[n]);
            let before = LOGS.map(|log| fs::read(dir.join(".alt").join(log)).unwrap());

            let (code, said, told) = fsck(dir);
            assert_eq!(code, Some(128), "{gone} {log}: {said}");
            assert!(!said.contains("unconfirmed tail"), "{gone} {log}: {said}");
            assert_eq!(told, Told::default(), "{gone} {log}");
            fs::write(dir.join("new.txt"), "new\n").unwrap();
            let wrote = alt(dir, &["add", "new.txt"]);
            assert!(!stderr(&wrote).contains("discarded"), "{log}: {wrote:?}");
            for (file, before) in LOGS.iter().zip(&before) {
                let now = fs::read(dir.join(".alt").join(file)).unwrap();
                assert!(now.starts_with(before), "{gone} {log}: {file} was cut");
            }
            assert_eq!(fsck(dir).0, Some(128), "{gone} {log}");
        }
    }
}

#[test]
fn a_command_the_daemon_runs_says_what_it_cut() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let (first, markers) = repo(dir);
    lag(dir, &markers);
    damage(dir, 0, first[0]);
    let (code, said, told) = fsck(dir);
    assert_eq!(code, Some(3), "{said}");

    fs::write(dir.join("new.txt"), "new\n").unwrap();
    let mut command = alt_command(dir, &["add", "new.txt"]);
    command.env_remove("ALT_NO_DAEMON");
    // the daemon the command starts goes away by itself
    let wrote = command.env("ALT_DAEMON_IDLE_MS", "1500").output().unwrap();
    assert!(wrote.status.success(), "{}", stderr(&wrote));
    let socket = dir.join(".alt/daemon.sock");
    assert!(
        std::os::unix::net::UnixStream::connect(socket).is_ok(),
        "the command did not go through a daemon"
    );
    assert_eq!(warned(&wrote), told, "{}", stderr(&wrote));
    assert_eq!(alt(dir, &["fsck"]).status.code(), Some(0));
}

/// Whole records and operations past the boundary that name content
/// which is not stored: a command that writes takes them, so the check
/// does not pass, and says what goes as that command then does.
#[test]
fn records_and_operations_whose_content_never_arrived_fail_the_check_as_the_writer_cuts_them() {
    for lost in [Lost::Everything, Lost::Payloads, Lost::NotesCommit] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let (first, second) = power_cut(dir, lost);
        let before = lens(dir);

        let (code, said, told) = fsck(dir);
        assert_eq!(code, Some(3), "{lost:?}: {said}");
        for note in [
            "1 operation(s) at the end of the operation log were never synced and name objects \
             that are not stored: the next command that writes discards them\n",
            "of records at the end of map.alt were never synced and name content that is not \
             stored: the next command that writes cuts them off\n",
            "every record before the unconfirmed tail verified\n",
            "fatal: fsck: unconfirmed tail: the next command that writes discards ",
            "copy the .alt directory first",
        ] {
            assert!(said.contains(note), "{lost:?}: {note}: {said}");
        }
        assert_eq!(told_by(&said, OBJECTS), Told::default(), "{lost:?}");
        assert!(told.0["map.alt"] > 0 && told.0["oplog/log"] > 0, "{said}");
        assert_eq!(told.1, 1, "{lost:?}: {said}");
        let main = format!("refs/heads/main: 1 update(s), from {first} to {second}");
        assert!(told.2.contains(&main), "{lost:?}: {said}");
        let json = alt(dir, &["fsck", "--json"]);
        assert_eq!(json.status.code(), Some(3), "{lost:?}");
        assert!(stderr(&json).contains("\"kind\":\"unconfirmed_tail\""));

        let wrote = alt(dir, &["tag", "after"]);
        assert!(wrote.status.success(), "{lost:?}: {}", stderr(&wrote));
        assert_eq!(warned(&wrote), told, "{lost:?}: {}", stderr(&wrote));
        assert_eq!(went(dir, before), told.0, "{lost:?}");

        let (code, said, told) = fsck(dir);
        assert_eq!((code, told), (Some(0), Told::default()), "{lost:?}: {said}");
        assert!(
            said.ends_with("every record verified\n"),
            "{lost:?}: {said}"
        );
        assert!(!said.contains("never"), "{lost:?}: {said}");
    }
}

/// The object store ends in a tail and the operation log holds nothing
/// past its boundary: a command that writes only a ref has no reason to
/// settle the object store, and leaves it. The check says the tail goes
/// with the next command that writes objects, and what the log ends in,
/// if anything, with the next that writes.
#[test]
fn a_tail_in_the_object_store_alone_waits_for_a_command_that_writes_objects() {
    for torn_log in [false, true] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        ok(alt(dir, &["init"]));
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        ok(alt(dir, &["add", "a.txt"]));
        ok(alt(dir, &["commit", "-m", "first"]));
        let first = lens(dir);
        let saved = ["odb.durable", "oplog/durable", "oplog/log"]
            .map(|file| fs::read(dir.join(".alt").join(file)).unwrap());
        for (name, seed) in [("b.bin", 1), ("c.bin", 2)] {
            fs::write(dir.join(name), noise(seed, 4000)).unwrap();
            ok(alt(dir, &["add", name]));
        }
        // the log as it was before the two adds, all of it synced
        fs::write(dir.join(".alt/oplog/log"), &saved[2]).unwrap();
        lag(dir, &[saved[0].clone(), saved[1].clone()]);
        damage(dir, 0, first[0]);
        if torn_log {
            let mut log = saved[2].clone();
            log.extend_from_slice(&[0xcd; 9]);
            fs::write(dir.join(".alt/oplog/log"), log).unwrap();
        }
        let before = lens(dir);
        let held = LOGS.map(|log| fs::read(dir.join(".alt").join(log)).unwrap());

        let (code, said, any) = fsck(dir);
        let objects = told_by(&said, OBJECTS);
        assert_eq!(code, Some(3), "{torn_log}: {said}");
        assert!(objects.0[LOGS[0]] > 0 && objects.0["map.alt"] > 0, "{said}");
        assert_eq!((objects.1, objects.2.len()), (0, 0), "{said}");
        let bytes: u64 = objects.0.values().sum();
        let fatal = match torn_log {
            true => format!(
                "fatal: fsck: unconfirmed tail: the next command that writes discards 9 byte(s), \
                 and the next command that writes objects discards {bytes} byte(s), that were \
                 never confirmed as synced; to keep them, copy the .alt directory first\n"
            ),
            false => format!(
                "fatal: fsck: unconfirmed tail: the next command that writes objects discards \
                 {bytes} byte(s) that were never confirmed as synced; to keep them, copy the \
                 .alt directory first\n"
            ),
        };
        assert!(said.contains(&fatal), "{torn_log}: {said}");
        assert!(!said.contains(" 0 operation(s)"), "{torn_log}: {said}");
        assert!(
            said.contains(
                "of records at the end of map.alt were never synced and name content that is \
                 not stored: the next command that writes objects cuts them off\n"
            ),
            "{torn_log}: {said}"
        );
        let log_end = "passed over 9 byte(s) at the end of oplog/log: an append that never \
                       finished; the next command that writes cuts them off\n";
        assert_eq!(said.contains(log_end), torn_log, "{said}");
        assert_eq!(said.contains(ANY), torn_log, "{said}");
        let torn: BTreeMap<_, _> = [("oplog/log".to_owned(), 9)].into();
        let expected = if torn_log { torn } else { BTreeMap::new() };
        assert_eq!(any, (expected, 0, Vec::new()), "{torn_log}: {said}");

        // a ref and nothing else: the object store's files stay
        let wrote = alt(dir, &["tag", "v1"]);
        assert!(wrote.status.success(), "{torn_log}: {}", stderr(&wrote));
        assert_eq!(warned(&wrote), any, "{torn_log}: {}", stderr(&wrote));
        assert_eq!(went(dir, before), any.0, "{torn_log}");
        let (code, said, any) = fsck(dir);
        assert_eq!(code, Some(3), "{torn_log}: {said}");
        assert_eq!(any, Told::default(), "{torn_log}: {said}");
        assert_eq!(told_by(&said, OBJECTS), objects, "{torn_log}: {said}");
        assert!(!said.contains(ANY), "{torn_log}: {said}");

        // an object: its files are cut where the check said
        let wrote = alt(dir, &["add", "b.bin"]);
        assert!(wrote.status.success(), "{torn_log}: {}", stderr(&wrote));
        assert_eq!(warned(&wrote), objects, "{torn_log}: {}", stderr(&wrote));
        for (file, held) in LOGS.iter().zip(&held).take(4) {
            let cut = objects.0.get(*file).copied().unwrap_or(0) as usize;
            let now = fs::read(dir.join(".alt").join(file)).unwrap();
            assert!(now.starts_with(&held[..held.len() - cut]), "{file}");
            assert_eq!(now.starts_with(held), cut == 0, "{torn_log}: {file}");
        }
        let (code, said, any) = fsck(dir);
        assert_eq!(
            (code, any),
            (Some(0), Told::default()),
            "{torn_log}: {said}"
        );
        assert!(said.ends_with("every record verified\n"), "{said}");
        assert!(!said.contains("never"), "{torn_log}: {said}");
    }
}
