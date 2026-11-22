//! A directory of boot runs that cannot be removed: a network file
//! system keeps a removed file that another process still maps as an
//! entry of its directory, which then does not go. The writer that takes
//! the boot runs away goes on, nobody looks in what is left, and it goes
//! once it can.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::boot_runs::{all_read, boot_runs, put_unsynced, reading, store};
use std::os::unix::fs::FileExt;

use super::rewind_interrupted::{FILES, lens, open, put, reads};

/// Makes `file` one that cannot be removed: the file system holds it as
/// immutable, or, where this user may not make it so, nothing can be
/// taken out of its directory. Returns what makes it removable again.
fn held_fast(file: &Path) -> impl FnOnce() + use<> {
    let flag = |flag: &str| {
        let set = Command::new("chattr").arg(flag).arg(file).output();
        set.is_ok_and(|set| set.status.success())
    };
    let immutable = flag("+i");
    let dir = file.parent().unwrap().to_owned();
    if !immutable {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    }
    assert!(fs::remove_file(file).is_err(), "the file can be removed");
    let file = file.to_owned();
    move || match immutable {
        true => {
            let freed = Command::new("chattr").arg("-i").arg(&file).status();
            assert!(freed.unwrap().success());
        }
        false => fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap(),
    }
}

/// Puts a file that cannot be removed into `dir`.
fn stuck(dir: &Path) -> (PathBuf, impl FnOnce() + use<>) {
    let file = dir.join(".nfs0001");
    fs::write(&file, b"mapped elsewhere").unwrap();
    let free = held_fast(&file);
    (file, free)
}

fn more() -> impl Iterator<Item = u32> {
    (2001..2900).filter(|n| n % 3 != 1)
}

#[test]
fn a_directory_of_boot_runs_that_cannot_be_removed_stops_no_writer_and_is_looked_in_by_nobody() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    store(dir);
    let runs = boot_runs(dir);
    assert_eq!(runs.len(), 3, "{runs:?}");
    let of_map = runs
        .iter()
        .find(|run| run.starts_with(dir.join("map.alt.idx")));
    let held = of_map.unwrap().parent().unwrap().to_owned();
    let (file, free) = stuck(&held);
    // the writer that syncs takes the note away, and of the files what
    // goes
    let mut writer = open(dir);
    put(&mut writer, 1503..1504);
    drop(writer);
    assert!(file.exists());
    let left = boot_runs(dir);
    assert!(left.iter().all(|run| run.starts_with(&held)), "{left:?}");
    let reader = reading(dir);
    all_read(&reader);
    reads(&reader, 1503);
    drop(reader);
    // a writer that leaves its appends unsynced, with boot runs to
    // write, and the next that syncs go on beside it
    put_unsynced(dir, more());
    let reader = reading(dir);
    all_read(&reader);
    more().for_each(|n| reads(&reader, n));
    drop(reader);
    let mut writer = open(dir);
    put(&mut writer, 3000..3001);
    drop(writer);
    assert!(file.exists());
    // once it can be removed, the next boot runs written take it away
    free();
    put_unsynced(dir, (4001..4900).filter(|n| n % 3 != 1));
    assert!(!file.exists());
    let reader = reading(dir);
    all_read(&reader);
    for n in more().chain([1503, 3000, 4001, 4898]) {
        reads(&reader, n);
    }
    drop(reader);
    let mut writer = open(dir);
    put(&mut writer, 5001..5002);
    drop(writer);
    assert_eq!(boot_runs(dir), Vec::<PathBuf>::new());
    open(dir).check().unwrap();
}

#[test]
fn a_boot_run_of_the_chunk_index_that_cannot_be_removed_is_not_looked_in_by_the_writer_that_syncs()
{
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let synced = store(dir);
    let ends = lens(dir);
    let runs = boot_runs(dir);
    let of_chunks = |run: &&PathBuf| run.starts_with(dir.join("store/chunks.idx"));
    let run = runs.iter().find(of_chunks).unwrap();
    let free = held_fast(run);
    // a page of the pack the batch appended is lost in its boot
    let at = (synced[0] + ends[0]) / 2;
    let at = at - at % 4096;
    let pack = fs::OpenOptions::new().write(true).open(dir.join(FILES[0]));
    pack.unwrap().write_all_at(&[0u8; 4096], at).unwrap();
    // the run stays where it is, as it was written; the writer that
    // syncs walks the pack all the same, and cuts it at the record the
    // page lies in
    let mut writer = open(dir);
    put(&mut writer, 1503..1504);
    drop(writer);
    assert!(run.exists());
    let after = lens(dir);
    assert!(after[0] > at && after[0] < at + 4096 + 400, "{after:?}");
    let reader = reading(dir);
    for n in (0..30).chain([1503]) {
        reads(&reader, n);
    }
    drop(reader);
    free();
    open(dir).check().unwrap();
}
