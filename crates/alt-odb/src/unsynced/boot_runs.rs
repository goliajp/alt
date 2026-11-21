//! The boot runs of the packs and the record logs: index runs over
//! appends that were left unsynced (see [`alt_store::runs::BootRun`]).
//!
//! Every open reads the records past the index runs of a log, and a
//! writer that leaves its appends unsynced builds no index run over
//! them: one `add` of ten thousand files would have each command after
//! it read ten thousand records, until a command syncs. Such a writer
//! writes a boot run for each log instead and notes them in the lock
//! file, after the notes of removals ([`crate::repair`]):
//!
//! ```text
//! [generation u64]
//! 4 x [start u64][end u64]      chunk index, blob map, tier1 map, map.alt
//! [checksum 8]                                                at byte 1312
//! ```
//!
//! A range of zeros says the log has none; the range of the chunk index
//! is one of positions in the packs, a pack and an offset in one number.
//! The files lie in `<log>.idx/<boot>-<generation>/`, those of the chunk
//! index in `store/chunks.idx/<boot>-<generation>/`, and neither they
//! nor the note are fsynced. An opener looks in them only while all
//! of this holds ([`held`]): the note of unsynced appends is of the
//! running boot, the marker says appends were left unsynced, and its
//! generation is the one noted here. After another boot the files may
//! hold anything; once records were taken out of a log — every cut by
//! this format moves the generation, and an older alt replaces the
//! marker by one that records none — a run may name a place where
//! another record has come to lie. The writer that finds the note not
//! holding takes it and the files away, and so does every writer that
//! syncs: the records are then indexed by the runs every opener reads.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_store::runs::BootRun;

use super::Left;
use crate::head::check;
use crate::repair::{LOGS, NOTED_AT, NOTES};
use crate::synced::{Log, Marker};

pub(crate) const AT: usize = NOTED_AT + NOTES;
const BODY: usize = 8 + 4 * 16;
pub(crate) const LEN: usize = BODY + 8;

/// The note: the generation of the marker the runs were written under,
/// and the log range of each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BootRuns {
    pub generation: u64,
    /// chunk index, blob map, Tier 1 map, `map.alt`
    pub runs: [(u64, u64); 4],
}

/// The directory boot runs written in `boot` under `generation` lie in,
/// under the index directory of their log.
pub(crate) fn dir(boot: [u8; 16], generation: u64) -> String {
    let boot: String = boot.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{boot}-{generation:016x}")
}

impl BootRuns {
    pub fn parse(data: &[u8]) -> Option<Self> {
        let (body, sum) = data.get(..LEN)?.split_at(BODY);
        if check(body) != *sum {
            return None;
        }
        let word = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
        Some(Self {
            generation: word(0),
            runs: [0, 1, 2, 3].map(|log| (word(8 + 16 * log), word(16 + 16 * log))),
        })
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(LEN);
        bytes.extend_from_slice(&self.generation.to_le_bytes());
        for (start, end) in self.runs {
            bytes.extend_from_slice(&start.to_le_bytes());
            bytes.extend_from_slice(&end.to_le_bytes());
        }
        let sum = check(&bytes);
        bytes.extend_from_slice(&sum);
        bytes
    }

    /// The boot run of `log` an opener in `boot` is handed.
    pub fn of(&self, log: Log, boot: [u8; 16]) -> Option<BootRun> {
        let (start, end) = match log {
            Log::Pack => self.runs[0],
            Log::Blobmap => self.runs[1],
            Log::Tier1 => self.runs[2],
            Log::Map => self.runs[3],
        };
        (end > start).then(|| BootRun {
            dir: dir(boot, self.generation),
            start,
            end,
        })
    }
}

/// Notes the boot runs; not fsynced, failures ignored: without the note
/// an opener reads the records.
pub(crate) fn note(lock: &File, runs: &BootRuns) {
    let _ = lock.write_all_at(&runs.encode(), AT as u64);
}

/// Takes the note away, and the boot runs of every log under `alt_dir`.
/// The caller holds the write lock.
pub(crate) fn forget(alt_dir: &Path, lock: &File) -> std::io::Result<()> {
    lock.write_all_at(&[0u8; LEN], AT as u64)?;
    let logs = LOGS.map(|log| {
        let mut idx = alt_dir.join(log).into_os_string();
        idx.push(".idx");
        idx
    });
    for idx in logs
        .into_iter()
        .chain([alt_dir.join("store/chunks.idx").into()])
    {
        alt_store::runs::remove_boot_runs(Path::new(&idx), None).map_err(std::io::Error::other)?;
    }
    Ok(())
}

/// The boot runs `noted` where they hold under `marker` and the note
/// `left` of unsynced appends, with the running boot.
pub(crate) fn held(
    marker: &Marker,
    left: Option<Left>,
    noted: Option<BootRuns>,
) -> Option<(BootRuns, [u8; 16])> {
    let runs = noted.filter(|runs| marker.generation() == runs.generation)?;
    let boot = alt_os::boot::id()?;
    super::cached(marker, left).then_some((runs, boot))
}

/// The same for an opener, which reads the marker only where the lock
/// file notes boot runs and unsynced appends.
pub(crate) fn held_at_open(
    durable: &Path,
    left: Option<Left>,
    noted: Option<BootRuns>,
) -> Option<(BootRuns, [u8; 16])> {
    noted.and(left)?;
    held(&Marker::read(durable), left, noted)
}
