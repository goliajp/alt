//! What becomes of `tail`, the note beside a driven version 2 log, while
//! a sequence runs: it is kept, meddled with before any read of it, or
//! never written. And where the driver takes from, with each, up to
//! where the log is recorded as synced.

use std::path::Path;
use std::sync::atomic::Ordering;

use super::v2_counts::Counts;
use super::v2_cut_told::no_boot;
use super::v2_driver::Run;
use super::v2_log::boot;
use super::v2_model::{Reference, Rng};

/// What becomes of `tail`, the note beside the log, while a sequence
/// runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Beside {
    /// it is as the handles leave it
    Kept,
    /// before any read of it, it may be removed or spoiled
    Meddled,
    /// the kernel tells no boot, and none is ever written
    Never,
}

/// Removes `tail` in `dir`, or changes a byte of it, or cuts it short,
/// once in eight times each time it is called, by the numbers of `rng`.
fn meddle(dir: &Path, rng: &mut Rng, counts: &Counts) {
    let path = dir.join("tail");
    match rng.below(24) {
        0 if std::fs::remove_file(&path).is_ok() => {
            counts.notes_gone.fetch_add(1, Ordering::Relaxed);
        }
        how @ 1..3 => {
            let Ok(mut noted) = std::fs::read(&path) else {
                return;
            };
            if noted.is_empty() {
                return;
            }
            let at = rng.below(noted.len() as u64) as usize;
            match how {
                1 => noted[at] ^= 1 << rng.below(8),
                _ => noted.truncate(at),
            }
            std::fs::write(&path, noted).unwrap();
            counts.notes_spoiled.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
}

/// Has `tail` meddled with before every read of it a handle of this
/// thread makes, by the numbers that follow from `seed`.
pub fn meddled(seed: u64, counts: &'static Counts) {
    let mut rng = Rng(seed ^ 0xbb67_ae85_84ca_a73b);
    let meddles = move |dir: &Path| meddle(dir, &mut rng, counts);
    alt_oplog::v2::before_note_read(Some(Box::new(meddles)));
}

impl Run {
    /// Has the handles this thread opens from now on run in boot `n`, as
    /// far as the kernel tells one where the sequence runs.
    pub fn in_boot(beside: Beside, n: u8) {
        match beside {
            Beside::Never => no_boot(),
            Beside::Kept | Beside::Meddled => boot(n),
        }
    }

    /// Up to where the log is recorded as synced. With the note beside
    /// it kept, that is what the note says. Where the note may be gone
    /// or is never written, it is the furthest any handle has it, or a
    /// frame of the log says: each of them was told so by a sync.
    pub fn claimed(&self, log: &Reference) -> u64 {
        let noted = alt_oplog::bounded_in(self.path()).unwrap_or(0);
        if self.beside == Beside::Kept {
            return noted;
        }
        let held = self.handles.iter().map(|handle| {
            let bounded = handle.log.bounded_to().unwrap_or(0);
            bounded.max(handle.log.synced_here())
        });
        held.fold(noted.max(log.said_synced), u64::max)
    }
}
