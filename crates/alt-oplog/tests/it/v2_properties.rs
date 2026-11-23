//! Twenty thousand sequences of what handles do to a version 2 log, on
//! blocks of three sizes (`v2_sequences`).

use std::sync::atomic::Ordering;

use super::v2_sequences::{BY_BLOCK, run_all};

/// Runs `sequences` sequences on blocks of two to the power of
/// `block_exp` bytes, and holds the run to having done each kind of
/// thing a sequence is drawn to do.
fn all_of(block_exp: u8, sequences: u64) {
    run_all(block_exp, sequences);
    let counts = &BY_BLOCK[(block_exp as usize - 10) / 2];
    let of = [
        ("sequences", &counts.sequences),
        ("cuts of ops nobody vouched for", &counts.cuts),
        ("handles told their ops were cut", &counts.cut_back),
        (
            "of them told so of ops the log held, a cut of them begun",
            &counts.cut_back_begun,
        ),
        (
            "things done while a writer had given up the lock",
            &counts.meanwhile,
        ),
        ("of them looks that cut the log", &counts.meanwhile_cuts),
        ("receipts", &counts.receipts),
        ("opens at an anchor that holds", &counts.anchored),
        (
            "opens at the anchor of an op that is gone",
            &counts.anchored_gone,
        ),
        ("cuts that died", &counts.cuts_died),
        ("logs grown back to the length they had", &counts.grown_back),
        (
            "of them after a cut that died",
            &counts.grown_back_after_death,
        ),
        (
            "reads that ended where the log was noted to end",
            &counts.hidden,
        ),
    ];
    let counted: Vec<String> = of
        .iter()
        .map(|(what, count)| format!("{} {what}", count.load(Ordering::Relaxed)))
        .collect();
    eprintln!("blocks of 2^{block_exp} bytes: {}", counted.join(", "));
    assert_eq!(counts.sequences.load(Ordering::Relaxed), sequences);
    for (what, count) in of {
        assert!(count.load(Ordering::Relaxed) > 0, "no {what}");
    }
}

#[test]
fn whatever_handles_do_none_takes_for_looked_at_or_synced_what_the_file_and_its_writers_do_not() {
    all_of(10, 10_000);
}

#[test]
fn whatever_handles_do_to_a_log_of_four_kibibyte_blocks_it_holds_what_its_writers_left_in_it() {
    all_of(12, 6_000);
}

#[test]
fn whatever_handles_do_to_a_log_of_sixteen_kibibyte_blocks_it_holds_what_its_writers_left_in_it() {
    all_of(14, 4_000);
}
