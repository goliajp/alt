//! The note that the index file was seen whole (`index.seen`) spares a
//! command the checksum of the index, and only of the file it was noted
//! for as it was then: an index file that was written into is checked
//! again.

use std::fs;
use std::os::unix::fs::FileExt;
use std::time::Duration;

use super::staged_durability::{add, repo};
use super::unconfirmed::{alt, ok, stderr};

#[test]
fn an_index_file_that_was_written_into_is_checked_though_it_was_noted_whole() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    add(dir, "b.txt");
    let noted = fs::read(dir.join(".alt/index.seen")).unwrap();
    assert!(!noted.is_empty());
    // an add that finds the file as it was noted goes by the note
    add(dir, "c.txt");
    assert_ne!(fs::read(dir.join(".alt/index.seen")).unwrap(), noted);

    // the note goes by the times of the file, which the clock sets in
    // ticks: the write below is in a later one than the add's
    std::thread::sleep(Duration::from_millis(50));
    let index = fs::OpenOptions::new()
        .write(true)
        .open(dir.join(".alt/index"));
    index.unwrap().write_all_at(&[0xff; 4], 60).unwrap();
    fs::write(dir.join("d.txt"), "d\n").unwrap();
    let added = alt(dir, &["add", "d.txt"]);
    let told = stderr(&added);
    assert!(told.contains("is not a whole index"), "{told}");
    ok(added);
    // the snapshot of the last add was that file: the index is the one
    // from before it, and what was added last has to be added again
    let status = ok(alt(dir, &["status"]));
    assert!(status.contains("new file:   b.txt"), "{status}");
    assert!(status.contains("new file:   d.txt"), "{status}");
    assert!(!status.contains("new file:   c.txt"), "{status}");
}
