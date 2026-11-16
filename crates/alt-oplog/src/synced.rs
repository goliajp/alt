//! The synced-length marker `durable`: how far the log has been fsynced.
//!
//! A committer about to fsync asks it whether another committer's fsync
//! already covered its appends. An open that finds a record it cannot read
//! asks it whether that record lies in the synced part of the log — damage,
//! reported with the file left as it is — or past it — an append that never
//! finished, cut off. Without the marker nothing is cut.
//!
//! ```text
//! [log eof u64][magic "ALTD"][whole u64][checksum 8]
//! ```
//!
//! The first number is the file length at the last fsync, all an older alt
//! reads or writes. `whole` is a record boundary known synced: a file
//! length can include a dead writer's unfinished record, a boundary cannot.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::OpLogError;

const MAGIC: [u8; 4] = *b"ALTD";

#[derive(Clone, Copy)]
pub(crate) struct Marker {
    /// file length at the last fsync
    pub eof: u64,
    /// record boundary known synced; none in a marker an older alt wrote
    pub whole: Option<u64>,
}

fn check(body: &[u8]) -> [u8; 8] {
    alt_hash::blake3::hash(body).as_bytes()[..8]
        .try_into()
        .unwrap()
}

impl Marker {
    pub fn read(path: &Path) -> Option<Self> {
        let data = std::fs::read(path).ok()?;
        let word = |at: usize| {
            Some(u64::from_le_bytes(
                data.get(at..at + 8)?.try_into().unwrap(),
            ))
        };
        let whole =
            (data.len() >= 28 && data[8..12] == MAGIC && check(&data[..20]) == data[20..28])
                .then(|| word(12).unwrap());
        Some(Self {
            eof: word(0)?,
            whole,
        })
    }

    /// Replaces the marker (temp + rename). Written only after the fsync it
    /// records, so it never claims more than is on disk.
    pub fn write(path: &Path, eof: u64, whole: u64) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(28);
        buf.extend_from_slice(&eof.to_le_bytes());
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&whole.to_le_bytes());
        let sum = check(&buf);
        buf.extend_from_slice(&sum);
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, buf)?;
        std::fs::rename(&tmp, path)
    }

    /// Whether an fsync covered appends up to the record boundary `target`
    /// and the marker says so.
    pub fn records(&self, target: u64) -> bool {
        self.eof >= target && self.whole.is_some_and(|whole| whole >= target)
    }

    /// Whether a record that does not read at `at` is an append that never
    /// finished rather than damage.
    pub fn unfinished(path: &Path, at: u64) -> bool {
        Self::read(path)
            .and_then(|marker| marker.whole)
            .is_some_and(|whole| at >= whole)
    }
}

/// An fsync handle for the log that works without the [`crate::OpLog`] it
/// came from (the daemon fsyncs off the write path): after the fsync it
/// records how far that log's deferred appends had reached before it.
pub struct OpLogSink {
    pub(crate) file: File,
    pub(crate) appended: Arc<AtomicU64>,
    pub(crate) durable_path: PathBuf,
}

impl OpLogSink {
    pub fn fsync(&self) -> Result<(), OpLogError> {
        let target = self.appended.load(Ordering::Acquire);
        self.file.sync_all()?;
        let marker = Marker::read(&self.durable_path);
        if target == 0 || marker.is_some_and(|marker| marker.records(target)) {
            return Ok(());
        }
        let eof = marker.map_or(0, |marker| marker.eof).max(target);
        let whole = marker.and_then(|marker| marker.whole).unwrap_or(0);
        Marker::write(&self.durable_path, eof, whole.max(target))?;
        Ok(())
    }
}
