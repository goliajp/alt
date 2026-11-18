//! The index runs an open builds over a store that has none: a cache a
//! command that only reads may write, over synced records and no others.

use std::fs;
use std::path::Path;

use super::super::unconfirmed::{alt, alt_command};
use super::super::unconfirmed_tails::{LOGS, lens};
use super::states::{Base, Step, run_dirs};
use super::{answer, tree};

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
