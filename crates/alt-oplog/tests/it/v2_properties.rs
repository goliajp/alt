//! Sixty thousand sequences of what handles do to a version 2 log, on
//! blocks of three sizes and of three batches of seeds (`v2_sequences`),
//! each with the note beside the log kept, meddled with before any read
//! of it, or never written. And as many more as the environment names,
//! of seeds of its choosing.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::Ordering;

use super::v2_counts::{COUNTS, Counts, NAMED};
use super::v2_driver::Run;
use super::v2_power::Power;

/// Runs the sequence of `steps` steps that `seed` gives on a log with
/// blocks of two to the power of `block_exp` bytes, and tells what was
/// done when a step of it fails.
fn run(seed: u64, block_exp: u8, steps: usize, counts: &'static Counts) {
    let fsyncs = alt_oplog::v2::fsyncs_asked();
    let mut run = Run::new(seed, block_exp, counts);
    let ran = catch_unwind(AssertUnwindSafe(|| {
        for _ in 0..steps {
            run.step();
            run.held_to_the_model();
        }
        // every caller asks at last, however long ago it wrote
        for index in 0..run.handles.len() {
            run.all_ask(index);
        }
        run.held_to_the_model();
    }));
    alt_oplog::v2::cut_dies_after(None);
    alt_oplog::v2::before_note_read(None);
    Power::off();
    if let Err(panic) = ran {
        eprintln!(
            "the sequence of seed {seed}, blocks of 2^{block_exp}, the note beside the log {:?}:\n{}",
            run.beside,
            run.trace.join("\n")
        );
        resume_unwind(panic);
    }
    let fsyncs = alt_oplog::v2::fsyncs_asked() - fsyncs;
    counts.fsyncs.fetch_add(fsyncs, Ordering::Relaxed);
    counts
        .real
        .fetch_add(run.power.real as u64, Ordering::Relaxed);
    counts.sequences.fetch_add(1, Ordering::Relaxed);
}

/// Runs the `sequences` sequences of the seeds from `first` on, on logs
/// with blocks of two to the power of `block_exp` bytes.
fn run_all(first: u64, block_exp: u8, sequences: u64, counts: &'static Counts) {
    const THREADS: u64 = 8;
    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            scope.spawn(move || {
                for seed in (first + thread..first + sequences).step_by(THREADS as usize) {
                    run(seed, block_exp, 40, counts);
                }
            });
        }
    });
}

/// Runs the `sequences` sequences of the seeds from `first` on, on
/// blocks of two to the power of `block_exp` bytes, and holds the run to
/// having done each kind of thing a sequence is drawn to do, where it is
/// of sequences enough for that.
fn held(first: u64, block_exp: u8, sequences: u64, counts: &'static Counts) {
    let began = std::time::Instant::now();
    run_all(first, block_exp, sequences, counts);
    let of = counts.named();
    let counted: Vec<String> = of
        .iter()
        .map(|(what, count)| format!("{} {what}", count.load(Ordering::Relaxed)))
        .collect();
    eprintln!(
        "seeds from {first}, blocks of 2^{block_exp} bytes, {:.1} seconds: {}",
        began.elapsed().as_secs_f64(),
        counted.join(", ")
    );
    assert_eq!(counts.sequences.load(Ordering::Relaxed), sequences);
    if sequences < 4_000 {
        return;
    }
    for (what, count) in of {
        // a cut is refused within where the log is synced a few times
        // in ten thousand sequences
        let seldom = what.starts_with("cuts refused within") && sequences < 10_000;
        assert!(seldom || count.load(Ordering::Relaxed) > 0, "no {what}");
    }
    // a cut refused for a frame to keep is one somebody needed, or of
    // one of the kinds the log cannot tell from that; those are fewer
    let count = |of: &std::sync::atomic::AtomicU64| of.load(Ordering::Relaxed);
    let needed = count(&counts.kept_needed);
    let unneeded = count(&counts.kept_uncovered);
    assert_eq!(needed + unneeded, count(&counts.kept_refused));
    assert!(
        unneeded < needed,
        "{unneeded} cuts refused that nobody needed"
    );
}

/// [`held`] for the sequences of batch `batch` of the seeds a gate runs.
fn all_of(batch: usize, block_exp: u8, sequences: u64) {
    let first = [0, 7_310_000, 1_124_000_000][batch];
    let counts = &COUNTS[batch][(block_exp as usize - 10) / 2];
    held(first, block_exp, sequences, counts);
}

#[test]
#[ignore = "runs the sequences ALT_OPLOG_SEQUENCES names, as `first seed:how many:block exponent`: \
            more of them, of other seeds, than a gate has the time for"]
fn the_sequences_the_environment_names_hold() {
    let named = std::env::var("ALT_OPLOG_SEQUENCES").expect("ALT_OPLOG_SEQUENCES is set");
    let mut parts = named.split(':').map(|part| part.parse::<u64>().unwrap());
    let mut next = || parts.next().expect("first seed:how many:block exponent");
    let (first, sequences, block_exp) = (next(), next(), next());
    held(first, block_exp as u8, sequences, &NAMED);
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
