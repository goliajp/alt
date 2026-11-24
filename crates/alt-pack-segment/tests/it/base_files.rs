//! The bases that memory has no room for are written to a directory of the
//! server's own, within what all the packs may take there; one that has no
//! room there, or cannot be written, is made again instead.

use super::waiting::{BYTES, pack};
use alt_pack_segment::{BaseFiles, Limits, SegmentSet, ingest};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

const DEPTH: usize = 200;

fn files(directory: &std::path::Path, most: u64) -> &'static BaseFiles {
    Box::leak(Box::new(BaseFiles::open(directory, most).unwrap()))
}

/// The index of the pack and the work it took, with room for five bases in
/// memory and for all of them in the files of this pack.
fn indexed(files: Option<&'static BaseFiles>) -> (Vec<u8>, alt_pack_segment::IndexWork) {
    indexed_by(files, None)
}

/// [`indexed`], waiting for what others have no longer than until `by`.
fn indexed_by(
    files: Option<&'static BaseFiles>,
    by: Option<Instant>,
) -> (Vec<u8>, alt_pack_segment::IndexWork) {
    let limits = Limits {
        base_bytes: 5 * BYTES,
        spill_bytes: 1 << 20,
        files,
        memory_by: by,
        ..Limits::of(1 << 20, 1 << 20, 1 << 30)
    };
    let ingested = ingest(&pack(DEPTH), &SegmentSet::default(), &limits).unwrap();
    let mut index = Vec::new();
    ingested.write_index(&mut index).unwrap();
    (index, ingested.work())
}

fn entries(directory: &std::path::Path) -> usize {
    std::fs::read_dir(directory).unwrap().count()
}

/// The directory is made for its owner alone, and what was left in it is removed.
/// With room for all the bases that wait, none is made twice, and nothing is left
/// in the directory or of the room when the pack is done.
#[test]
fn the_bases_in_files_are_in_the_directory_they_are_given_and_gone_with_the_pack() {
    let temp = alt_os::temp::tempdir().unwrap();
    let directory = temp.path().join("bases");
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir(directory.join("left")).unwrap();
    std::fs::write(directory.join("left/base"), b"of a process that was ended").unwrap();
    std::fs::write(directory.join("file"), b"and this").unwrap();
    let files = files(&directory, 1 << 20);
    let mode = std::fs::metadata(&directory).unwrap().permissions().mode();
    assert_eq!((mode & 0o777, entries(&directory)), (0o700, 0));

    let (index, work) = indexed(Some(files));
    assert!(work.spilled >= ((DEPTH - 6) * BYTES) as u64, "{work:?}");
    assert_eq!(work.applied, 2 * DEPTH as u64);
    assert_eq!(index, indexed(None).0);
    let (now, most) = files.taken();
    assert_eq!((now, most, files.unwritten()), (0, work.spilled, 0));
    assert_eq!(entries(&directory), 0);
    // a directory that is not there yet is made
    let made = directory.with_file_name("made");
    self::files(&made, 1);
    let mode = std::fs::metadata(&made).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}

/// All the packs have room for three bases in files between them: no more are
/// there at once, whichever pack they are of, and the bases there is no room for
/// are made again. The packs are indexed the same.
#[test]
fn the_packs_indexed_at_once_take_what_they_have_between_them_and_make_the_rest_again() {
    let work = alt_os::temp::tempdir().unwrap();
    let files = files(&work.path().join("bases"), 3 * BYTES as u64);
    let expected = indexed(None).0;
    let packs: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(move || indexed(Some(files))))
        .collect();
    for pack in packs {
        let (index, work) = pack.join().unwrap();
        assert_eq!(index, expected);
        assert!(work.applied > 2 * DEPTH as u64, "{work:?}");
    }
    let (now, most) = files.taken();
    assert!(now == 0 && most > 0 && most <= 3 * BYTES as u64, "{most}");
}

/// The directory is gone when the bases are to be written: none can be, each is
/// counted, and the pack is indexed all the same, its bases made again.
#[test]
fn a_base_that_cannot_be_written_is_made_again() {
    let temp = alt_os::temp::tempdir().unwrap();
    let directory = temp.path().join("bases");
    let files = files(&directory, 1 << 20);
    std::fs::remove_dir(&directory).unwrap();
    let (index, work) = indexed(Some(files));
    assert_eq!(index, indexed(None).0);
    assert_eq!(work.spilled, 0);
    assert!(work.applied > 2 * DEPTH as u64, "{work:?}");
    assert!(files.unwritten() > 0 && files.taken().0 == 0);
}

/// A pack that has no base in files waits for the room another has there: until
/// it is given back, and no longer than it waits for anything. One that has some
/// there does not wait for more, which may be its own to give back.
#[test]
fn a_pack_with_no_base_in_files_waits_for_room_there_and_one_with_some_does_not() {
    let work = alt_os::temp::tempdir().unwrap();
    let files = files(&work.path().join("bases"), 5 * BYTES as u64);
    let expected = indexed(None).0;

    // another has all of it for as long as this pack waits: it makes its bases again
    let other = files.occupy(5 * BYTES as u64).unwrap();
    let began = Instant::now();
    let wait = Duration::from_millis(400);
    let (index, took) = indexed_by(Some(files), Some(began + wait));
    assert!(began.elapsed() >= wait, "{:?}", began.elapsed());
    assert_eq!((index, took.spilled), (expected.clone(), 0));
    assert!(took.applied > 2 * DEPTH as u64, "{took:?}");

    // given back while the pack waits: it goes on with its bases in files
    let long = Instant::now() + Duration::from_secs(60);
    let (index, took) = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| indexed_by(Some(files), Some(long)));
        while files.waiting() == 0 {
            assert!(!waiting.is_finished(), "the pack did not wait for room");
            std::thread::yield_now();
        }
        drop(other);
        waiting.join().unwrap()
    });
    assert_eq!(index, expected);
    assert!(took.spilled > 0, "{took:?}");

    // Room for one base, which the pack takes: the next has none, and waiting for
    // it would be waiting for this pack itself.
    let files = self::files(&work.path().join("one"), BYTES as u64);
    let began = Instant::now();
    let (index, took) = indexed_by(Some(files), Some(began + Duration::from_secs(30)));
    assert!(
        began.elapsed() < Duration::from_secs(15),
        "{:?}",
        began.elapsed()
    );
    assert_eq!((index, took.spilled), (expected, BYTES as u64));
    assert_eq!(files.taken(), (0, BYTES as u64));
}

/// More bases of one pack wait than a pack may have in files at once, each of
/// a thousand bytes: as many are in files as may be, the rest are made again,
/// and the pack is indexed the same. Where the packs between them have ten
/// files left of those there may be, the pack has ten.
#[test]
fn a_pack_has_so_many_bases_in_files_at_once_and_all_the_packs_so_many() {
    use alt_pack_segment::{BASE_FILES, SPILL_FILES};
    const LONG: usize = SPILL_FILES + 500;
    let pack = pack(LONG);
    let indexed = |files: Option<&'static BaseFiles>| {
        let limits = Limits {
            base_bytes: 5 * BYTES,
            spill_bytes: 1 << 30,
            files,
            ..Limits::of(1 << 20, 1 << 20, 1 << 32)
        };
        let ingested = ingest(&pack, &SegmentSet::default(), &limits).unwrap();
        let mut index = Vec::new();
        ingested.write_index(&mut index).unwrap();
        (index, ingested.work())
    };
    let work = alt_os::temp::tempdir().unwrap();
    let files = files(&work.path().join("bases"), 1 << 30);
    let (expected, in_memory) = indexed(None);
    let (index, took) = indexed(Some(files));
    assert_eq!(index, expected);
    assert_eq!(in_memory.spilled_files, SPILL_FILES, "{in_memory:?}");
    assert_eq!(took.spilled_files, SPILL_FILES, "{took:?}");
    assert!(took.applied > 2 * LONG as u64, "{took:?}");
    assert_eq!(files.files(), (0, SPILL_FILES));

    let others: Vec<_> = (0..BASE_FILES - 10)
        .map(|_| files.occupy(1).unwrap())
        .collect();
    let (index, took) = indexed(Some(files));
    assert_eq!((index, took.spilled_files), (expected, 10), "{took:?}");
    assert_eq!(files.files().1, BASE_FILES);
    drop(others);
    assert_eq!(files.files().0, 0);
}
