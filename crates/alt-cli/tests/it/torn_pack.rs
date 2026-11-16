//! A power cut during `alt add`: the headers of the new pack records
//! reached the disk and their payloads did not, or zeros did where records
//! were to follow — or the record of `map.alt` that names the new blob
//! reached the disk and the blob did not. The repository reads as it did
//! before the add, the file is added again, and the full check passes.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const PACK: &str = ".alt/store/pack-00000001.altpack";

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env_remove("ALT_RELAXED_DURABILITY")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// A repository with three commits.
fn repo(dir: &Path) {
    ok(alt(dir, &["init"]));
    for tag in ["one", "two", "three"] {
        for n in 0..3 {
            fs::write(dir.join(format!("{tag}-{n}.txt")), format!("{tag} {n}\n")).unwrap();
        }
        ok(alt(dir, &["add", "."]));
        ok(alt(dir, &["commit", "-m", tag]));
    }
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Text that does not compress away, so its record has a payload to lose.
fn precious() -> String {
    let mut word = 0x9e37_79b9_7f4a_7c15u64;
    (0..400)
        .map(|_| {
            word ^= word << 13;
            word ^= word >> 7;
            word ^= word << 17;
            format!("{word:016x}\n")
        })
        .collect()
}

/// Zeroes the payload of every record of `pack` that begins at or past
/// `from`; returns how many there were.
fn zero_payloads(pack: &mut [u8], from: usize) -> usize {
    let (mut at, mut records) = (from, 0);
    while at + 41 <= pack.len() {
        let stored = u32::from_le_bytes(pack[at + 37..at + 41].try_into().unwrap()) as usize;
        pack[at + 41..at + 41 + stored].fill(0);
        at += 41 + stored;
        records += 1;
    }
    records
}

fn len(path: &Path) -> u64 {
    fs::metadata(path).unwrap().len()
}

#[test]
fn a_file_whose_add_lost_its_payloads_to_a_power_cut_is_added_again_and_restored() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    fs::create_dir(&dir).unwrap();
    repo(&dir);
    ok(alt(&dir, &["branch", "without"]));
    let pack = dir.join(PACK);
    let synced = len(&pack) as usize;
    let saved = tmp.path().join("saved");
    copy(&dir.join(".alt"), &saved);
    fs::write(dir.join("precious.txt"), precious()).unwrap();
    ok(alt(&dir, &["add", "precious.txt"]));
    // everything but the pack is as it was before the add
    let mut torn = fs::read(&pack).unwrap();
    assert!(zero_payloads(&mut torn, synced) > 0);
    fs::remove_dir_all(dir.join(".alt")).unwrap();
    copy(&saved, &dir.join(".alt"));
    fs::write(&pack, &torn).unwrap();

    ok(alt(&dir, &["fsck"]));
    assert!(ok(alt(&dir, &["status"])).contains("precious.txt"));
    assert_eq!(
        fs::read(&pack).unwrap(),
        torn,
        "a command that reads cut the pack"
    );
    ok(alt(&dir, &["add", "precious.txt"]));
    ok(alt(&dir, &["commit", "-m", "precious"]));
    assert!(ok(alt(&dir, &["log", "-n", "1"])).contains("precious"));
    // written out of the store again by a switch away and back
    ok(alt(&dir, &["branch", "with"]));
    ok(alt(&dir, &["switch", "without"]));
    assert!(!dir.join("precious.txt").exists());
    ok(alt(&dir, &["switch", "with"]));
    assert_eq!(
        fs::read_to_string(dir.join("precious.txt")).unwrap(),
        precious()
    );
    ok(alt(&dir, &["fsck"]));
}

/// The kernel wrote `map.alt` out before the pack: the record that names
/// the new blob is whole on disk, and the blob never arrived or arrived as
/// zeros. The next add cuts the record off and stores the file again.
#[test]
fn a_file_whose_add_reached_the_map_and_not_the_pack_is_added_again_and_restored() {
    for zeroed in [false, true] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path().join("repo");
        fs::create_dir(&dir).unwrap();
        repo(&dir);
        ok(alt(&dir, &["branch", "without"]));
        let (pack, map) = (dir.join(PACK), dir.join(".alt/map.alt"));
        let synced = len(&pack) as usize;
        let saved = tmp.path().join("saved");
        copy(&dir.join(".alt"), &saved);
        fs::write(dir.join("precious.txt"), precious()).unwrap();
        ok(alt(&dir, &["add", "precious.txt"]));
        let named = fs::read(&map).unwrap();
        let mut torn = fs::read(&pack).unwrap();
        match zeroed {
            true => assert!(zero_payloads(&mut torn, synced) > 0),
            false => torn.truncate(synced),
        }
        // everything but the map is as it was before the add
        fs::remove_dir_all(dir.join(".alt")).unwrap();
        copy(&saved, &dir.join(".alt"));
        fs::write(&pack, &torn).unwrap();
        assert!(named.len() as u64 > len(&map));
        fs::write(&map, &named).unwrap();

        assert!(ok(alt(&dir, &["status"])).contains("precious.txt"));
        let _ = alt(&dir, &["fsck"]);
        assert_eq!(fs::read(&pack).unwrap(), torn, "a read cut the pack");
        assert_eq!(fs::read(&map).unwrap(), named, "a read cut the map");
        ok(alt(&dir, &["add", "precious.txt"]));
        ok(alt(&dir, &["commit", "-m", "precious"]));
        assert!(ok(alt(&dir, &["log", "-n", "1"])).contains("precious"));
        ok(alt(&dir, &["branch", "with"]));
        ok(alt(&dir, &["switch", "without"]));
        assert!(!dir.join("precious.txt").exists());
        ok(alt(&dir, &["switch", "with"]));
        assert_eq!(
            fs::read_to_string(dir.join("precious.txt")).unwrap(),
            precious()
        );
        ok(alt(&dir, &["fsck"]));
    }
}

#[test]
fn zeros_a_power_cut_left_after_the_last_record_are_passed_over_and_cut_by_the_next_write() {
    let tmp = alt_os::temp::tempdir().unwrap();
    repo(tmp.path());
    ok(alt(tmp.path(), &["branch", "without"]));
    let pack = tmp.path().join(PACK);
    let synced = len(&pack);
    let mut zeros = fs::read(&pack).unwrap();
    zeros.extend_from_slice(&[0; 4096]);
    fs::write(&pack, &zeros).unwrap();

    ok(alt(tmp.path(), &["fsck"]));
    assert!(ok(alt(tmp.path(), &["log", "-n", "1"])).contains("three"));
    assert_eq!(
        fs::read(&pack).unwrap(),
        zeros,
        "a command that reads cut the pack"
    );
    fs::write(tmp.path().join("four.txt"), "four\n").unwrap();
    ok(alt(tmp.path(), &["add", "four.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "four"]));
    let after = fs::read(&pack).unwrap();
    assert_eq!(after[..synced as usize], zeros[..synced as usize]);
    assert!(after[synced as usize..synced as usize + 32] != [0; 32]);
    ok(alt(tmp.path(), &["fsck"]));
    ok(alt(tmp.path(), &["branch", "with"]));
    ok(alt(tmp.path(), &["switch", "without"]));
    assert!(!tmp.path().join("four.txt").exists());
    ok(alt(tmp.path(), &["switch", "with"]));
    assert_eq!(
        fs::read_to_string(tmp.path().join("four.txt")).unwrap(),
        "four\n"
    );
}
