//! Sixty thousand sequences of what handles do to a version 2 log, on
//! blocks of three sizes and of three batches of seeds (`v2_sequences`),
//! each with the note beside the log kept, meddled with before any read
//! of it, or never written.

use std::sync::atomic::Ordering;

use super::v2_counts::COUNTS;
use super::v2_sequences::run_all;

/// Runs the `sequences` sequences of the seeds of batch `batch` on
/// blocks of two to the power of `block_exp` bytes, and holds the run to
/// having done each kind of thing a sequence is drawn to do.
fn all_of(batch: usize, block_exp: u8, sequences: u64) {
    let first = [0, 7_310_000, 1_124_000_000][batch];
    let counts = &COUNTS[batch][(block_exp as usize - 10) / 2];
    run_all(first, block_exp, sequences, counts);
    let of = counts.named();
    let counted: Vec<String> = of
        .iter()
        .map(|(what, count)| format!("{} {what}", count.load(Ordering::Relaxed)))
        .collect();
    eprintln!(
        "seeds from {first}, blocks of 2^{block_exp} bytes: {}",
        counted.join(", ")
    );
    assert_eq!(counts.sequences.load(Ordering::Relaxed), sequences);
    for (what, count) in of {
        assert!(count.load(Ordering::Relaxed) > 0, "no {what}");
    }
}

#[test]
fn whatever_handles_do_none_takes_for_looked_at_or_synced_what_the_file_and_its_writers_do_not() {
    all_of(0, 10, 10_000);
}

#[test]
fn whatever_handles_do_to_a_log_of_four_kibibyte_blocks_it_holds_what_its_writers_left_in_it() {
    all_of(0, 12, 6_000);
}

#[test]
fn whatever_handles_do_to_a_log_of_sixteen_kibibyte_blocks_it_holds_what_its_writers_left_in_it() {
    all_of(0, 14, 4_000);
}

#[test]
fn the_sequences_of_a_second_batch_of_seeds_hold_on_blocks_of_one_kibibyte() {
    all_of(1, 10, 10_000);
}

#[test]
fn the_sequences_of_a_second_batch_of_seeds_hold_on_blocks_of_four_kibibytes() {
    all_of(1, 12, 6_000);
}

#[test]
fn the_sequences_of_a_second_batch_of_seeds_hold_on_blocks_of_sixteen_kibibytes() {
    all_of(1, 14, 4_000);
}

#[test]
fn the_sequences_of_a_third_batch_of_seeds_hold_on_blocks_of_one_kibibyte() {
    all_of(2, 10, 10_000);
}

#[test]
fn the_sequences_of_a_third_batch_of_seeds_hold_on_blocks_of_four_kibibytes() {
    all_of(2, 12, 6_000);
}

#[test]
fn the_sequences_of_a_third_batch_of_seeds_hold_on_blocks_of_sixteen_kibibytes() {
    all_of(2, 14, 4_000);
}
