//! The index runs an open builds over a store that has none: a cache a
//! command that only reads may write, over synced records and no others.

use std::fs;
use std::path::Path;

use super::super::unconfirmed::{alt, alt_command, ok};
use super::super::unconfirmed_tails::{LOGS, MARKERS, archive, lag, lens, noise};
use super::states::{Base, Step, copy_tree, run_dirs};
use super::{answer, answers_as_after_the_cut, tree};

/// Where the index runs beside `log` stop: the byte of the log (of the
/// active pack, for the chunk index) the last of them indexes up to.
fn indexed_to(dir: &Path, log: &str) -> Option<u64> {
    let runs = match log {
        "store/pack-00000001.altpack" => dir.join(".alt/store/chunks.idx"),
        log => dir.join(".alt").join(format!("{log}.idx")),
    };
    let names = fs::read_dir(runs).ok()?.map(|run| run.unwrap().file_name());
    let ends = names.filter_map(|name| {
        let name = name.into_string().unwrap();
        let end = name.strip_suffix(".run")?.split_once('-')?.1.to_owned();
        // a position in the chunk index carries the pack above 40 bits
        Some(u64::from_str_radix(&end, 16).unwrap() & ((1 << 40) - 1))
    });
    ends.max()
}

#[test]
fn the_index_runs_a_reader_builds_are_over_synced_records_and_no_others() {
    use Step::*;
    let (pack, map) = (LOGS[0], "map.alt");
    let base = Base::build();
    let ends = lens(&base.state("ends", &[]));
    // what each marker state says was synced of the pack and of map.alt
    let cases: [(&[Step], Option<[u64; 2]>); 6] = [
        (&[NoRuns], Some([ends[0], ends[3]])),
        (&[NoRuns, OlderMarkers], Some([ends[0], ends[3]])),
        (&[NoRuns, Lag], Some([base.first[0], base.first[3]])),
        (
            &[NoRuns, Lag, Damage(0)],
            Some([base.first[0], base.first[3]]),
        ),
        (&[NoRuns, NeverMarked], None),
        (&[NoRuns, EmptyMarkers], None),
    ];
    for (n, (steps, synced)) in cases.iter().enumerate() {
        let dir = base.state(&format!("runs{n}"), steps);
        // the check builds none
        let checked = alt(&dir, &["fsck"]);
        assert!(run_dirs(&dir.join(".alt")).is_empty(), "{steps:?}");
        let _ = alt(&dir, &["cat-file", "-p", "HEAD"]);
        let built = [indexed_to(&dir, pack), indexed_to(&dir, map)];
        match synced {
            Some(synced) => assert_eq!(built, synced.map(Some), "{steps:?}"),
            None => assert_eq!(built, [None, None], "{steps:?}"),
        }
        // the runs are whole: no later reader writes one, and the check
        // says of the repository what it said without them
        let before = tree(&dir);
        let _ = alt(&dir, &["log", "-n", "2"]);
        let after = tree(&dir);
        assert!(before == after, "{steps:?}: a second reader wrote again");
        let again = alt(&dir, &["fsck"]);
        assert_eq!(answer(&again), answer(&checked), "{steps:?}");
    }
}

/// A process that reads past damage changes no file: it builds no index
/// run either, where the same command without that would.
#[test]
fn a_reader_that_reads_past_damage_builds_no_index_run() {
    let base = Base::build();
    let dir = base.state("past", &[Step::NoRuns]);
    let before = tree(&dir);
    let mut past = alt_command(&dir, &["cat-file", "-p", "HEAD"]);
    let read = past.env("ALT_READ_PAST_DAMAGE", "1").output().unwrap();
    assert!(read.status.success(), "{read:?}");
    assert!(tree(&dir) == before, "reading past damage changed a file");
    assert!(run_dirs(&dir.join(".alt")).is_empty());
    let _ = alt(&dir, &["cat-file", "-p", "HEAD"]);
    assert!(!run_dirs(&dir.join(".alt")).is_empty(), "nothing to build");
}

/// A run is written once its records are fsynced and before the marker
/// says so, and can reach past the boundary. With the content of the
/// records past it gone, a reader answers behind the run, as it does
/// once a writer cut them.
#[test]
fn a_reader_answers_as_after_the_cut_where_an_index_run_reaches_past_the_boundary() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let base = tmp.path().join("base");
    fs::create_dir(&base).unwrap();
    ok(alt(&base, &["init"]));
    fs::write(base.join("a.txt"), "one\n").unwrap();
    ok(alt(&base, &["add", "."]));
    ok(alt(&base, &["commit", "-m", "first"]));
    let first = lens(&base);
    let first_head = ok(alt(&base, &["rev-parse", "HEAD"]));
    let markers = MARKERS.map(|marker| fs::read(base.join(".alt").join(marker)).unwrap());
    for n in 0..300 {
        fs::write(base.join(format!("f{n}.txt")), format!("file {n}\n")).unwrap();
    }
    fs::write(base.join("l.bin"), noise(1, 1 << 20)).unwrap();
    fs::write(base.join("z.zip"), archive(&noise(9, 4000))).unwrap();
    ok(alt(&base, &["add", "."]));
    ok(alt(&base, &["commit", "-m", "second"]));
    assert!(indexed_to(&base, "map.alt").unwrap() > first[3]);
    // the pack, then the blob map, end where they did at the boundary
    for lost in [0, 1] {
        let (found, written) = (tmp.path().join("found"), tmp.path().join("written"));
        for dir in [&found, &written] {
            let _ = fs::remove_dir_all(dir);
            copy_tree(&base, dir);
            lag(dir, &markers);
            let log = fs::OpenOptions::new()
                .write(true)
                .open(dir.join(".alt").join(LOGS[lost]))
                .unwrap();
            log.set_len(first[lost]).unwrap();
        }
        let head = alt(&found, &["rev-parse", "HEAD"]);
        assert_eq!(ok(head), first_head, "{} lost", LOGS[lost]);
        let said = answers_as_after_the_cut(&found, &written, LOGS[lost]);
        let (cut, discarded, left_out) = said.expect("the writer cuts");
        assert!(
            cut && discarded > 0,
            "{}: {discarded} discarded",
            LOGS[lost]
        );
        // a command that reads the blobs the index still names fails,
        // before the cut and after it alike
        assert!(left_out >= 4, "{}: {left_out} readers", LOGS[lost]);
    }
}
