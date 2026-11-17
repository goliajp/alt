//! Operations that were never synced and name content that is not stored.
//!
//! What an op names (objects, for a ref transaction) is written and synced
//! before the op is, but the op is in the page cache before that sync is
//! over, and the kernel writes pages out in whatever order it likes. A
//! power cut can leave the op whole in the log while what it names never
//! arrived. Such an op starts past the boundary the synced marker records:
//! nobody was told it happened. The log does not know what an op names;
//! whoever does passes `held`, which says whether everything an op names
//! is there.
//!
//! A writer cuts the log from the first such op on ([`OpLog::settle`]). A
//! reader changes nothing and can be told where that op starts
//! ([`OpLog::unconfirmed`]), to open the log again as it stood before it
//! ([`end_reads_at`]). An op before the boundary that names content which
//! is not stored is damage and is neither looked for nor cut here, and
//! where the marker records no boundary that holds, no op is looked at.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::lock::{lock_exclusive, unlock};
use crate::read::{read_at, read_log, scan};
use crate::scan::{CHECKSUM_LEN, HEADER_LEN};
use crate::synced::{self, Marker};
use crate::{Op, OpLog, OpLogError};

/// The ops at the end of the log from the first one on that was never
/// synced and names content that is not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unconfirmed {
    /// where the record of the first of them starts
    pub at: u64,
    /// how many there are
    pub ops: usize,
}

/// The log file opens of this process stop reading at, and where.
static READ_END: OnceLock<(u64, u64, u64)> = OnceLock::new();

/// Makes every later open of the log in `dir` by this process read it only
/// up to byte `at`, as if it ended there. For a command that reads: the
/// first call stands, the file is not touched.
#[cfg(unix)]
pub fn end_reads_at(dir: &Path, at: u64) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(dir.join("log"))?;
    let _ = READ_END.set((meta.dev(), meta.ino(), at));
    Ok(())
}

/// How far an open reads the log file `meta` is of.
#[cfg(unix)]
pub(crate) fn read_end(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    match READ_END.get() {
        Some(&(dev, ino, at)) if dev == meta.dev() && ino == meta.ino() => at.min(meta.len()),
        _ => meta.len(),
    }
}

#[cfg(not(unix))]
pub fn end_reads_at(_dir: &Path, _at: u64) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn read_end(meta: &std::fs::Metadata) -> u64 {
    meta.len()
}

/// The log and the bytes of it an open of this process did not read.
static PASSED_DAMAGE: OnceLock<(PathBuf, u64, u64)> = OnceLock::new();

/// Whether this process reads the log past damage: by
/// `ALT_READ_PAST_DAMAGE` in its environment, for a process that only
/// reads. An open then takes the log to end where a record is damaged,
/// and notes it ([`passed_damage`]), instead of failing. The environment
/// is asked each time, which is once an open.
pub fn reading_past_damage() -> bool {
    std::env::var_os("ALT_READ_PAST_DAMAGE").is_some()
}

/// Notes that the log beside the marker `durable` was not read from byte
/// `from` to its end at `to`.
pub(crate) fn note_damage(durable: &Path, from: u64, to: u64) {
    let _ = PASSED_DAMAGE.set((durable.with_file_name("log"), from, to));
}

/// The log file and the bytes of it, from a damaged record to the end of
/// the file, that an open of this process did not read.
pub fn passed_damage() -> Option<(PathBuf, u64, u64)> {
    PASSED_DAMAGE.get().cloned()
}

fn record_len(op: &Op) -> u64 {
    // length, the fixed part of a body, the checksum
    (4 + 78 + op.actor.len() + op.payload.len() + CHECKSUM_LEN) as u64
}

impl OpLog {
    /// For the full check: where the ops held end, how long the file is,
    /// and where in between a whole record begins whose checksum holds.
    /// A reader passes over an append that never finished at the end of
    /// the log; a whole record there is one it must not pass over. A
    /// record may begin at any byte, so every byte is tried: one hash of
    /// what would be the record for each byte whose length field fits.
    pub fn passed(&self) -> Result<(u64, u64, Option<u64>), OpLogError> {
        let (holds, len) = (self.len_bytes, self.file.metadata()?.len());
        let rest = read_at(&self.file, holds, len.saturating_sub(holds) as usize)?;
        let whole = (0..rest.len()).find(|&at| {
            let Some(body_len) = rest.get(at..at + 4) else {
                return false;
            };
            let body_len = u32::from_le_bytes(body_len.try_into().unwrap()) as usize;
            let record = rest.get(at + 4..(at + 4 + body_len).saturating_add(CHECKSUM_LEN));
            record.is_some_and(|record| {
                let (body, check) = record.split_at(body_len);
                // the fixed part of a body
                body_len >= 78 && alt_hash::blake3::hash(body).as_bytes()[..CHECKSUM_LEN] == *check
            })
        });
        Ok((holds, len, whole.map(|at| holds + at as u64)))
    }

    /// Whether the open stopped short of the file's end ([`end_reads_at`]).
    pub fn clamped(&self) -> bool {
        self.clamped
    }

    /// How many times this handle has read the log anew since it was
    /// opened: its ops are then other ones, from the first record on.
    pub fn reads(&self) -> u64 {
        self.reads
    }

    /// Whether the marker says every op this handle holds is synced.
    /// Reads the marker and takes no lock.
    pub fn confirmed(&self) -> bool {
        let marker = Marker::read(&self.durable_path);
        marker.trusted() && marker.records(self.len_bytes)
    }

    /// Whether ops this handle holds lie past a boundary the marker
    /// records: those a writer looks at before it appends
    /// ([`Self::settle`]). Without such a boundary there are none to look
    /// at. Reads the marker and takes no lock.
    pub fn unsettled(&self) -> bool {
        let from = Marker::read(&self.durable_path).unsynced_from();
        from.is_some_and(|from| from < self.len_bytes)
    }

    /// Whether the record of the last op this handle holds is still where
    /// the handle has it, in a file of `size` bytes: a cut of whole
    /// records takes it away, and what is appended after reads otherwise.
    pub(crate) fn holds_head(&self, size: u64) -> Result<bool, OpLogError> {
        let Some(head) = self.head() else {
            return Ok(true);
        };
        if size < self.len_bytes {
            return Ok(false);
        }
        let check = read_at(
            &self.file,
            self.len_bytes - CHECKSUM_LEN as u64,
            CHECKSUM_LEN,
        )?;
        Ok(check == head.0[..CHECKSUM_LEN])
    }

    /// Whether this handle fsyncs each op it appends before it appends
    /// the next: a cut of the log, which takes no synced op, then takes
    /// none of this handle's.
    pub fn syncs_inline(&self) -> bool {
        !self.defer && !crate::relaxed_durability()
    }

    /// Whether this handle appended to the log: a drop of it then syncs
    /// the log once more.
    pub fn appended(&self) -> bool {
        self.unsynced
    }

    /// Reads the log again from its first record, for a handle whose
    /// records were cut ([`OpLogError::CutBack`]). Whoever derived state
    /// from the ops of this handle derives it again.
    pub fn reopen(&mut self) -> Result<(), OpLogError> {
        crate::lock::lock_shared(&self.file)?;
        let reopened = self.reopen_locked();
        let _ = unlock(&self.file);
        reopened
    }

    /// The first op at or past byte `from` that `held` does not vouch for,
    /// with the ops after it.
    fn first_unheld<E: From<OpLogError>>(
        &self,
        from: u64,
        mut held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        let recent_from = self.base.map_or(HEADER_LEN as u64, |anchor| anchor.end);
        let (ops, mut at) = if from < recent_from {
            (self.ops()?, HEADER_LEN as u64)
        } else {
            (&self.ops[..], recent_from)
        };
        for (index, op) in ops.iter().enumerate() {
            if at >= from && !held(op)? {
                return Ok(Some(Unconfirmed {
                    at,
                    ops: ops.len() - index,
                }));
            }
            at += record_len(op);
        }
        Ok(None)
    }

    /// For a reader: the ops at the end of the log, as this handle holds
    /// it, that a writer would cut ([`Self::settle`]). Reads the marker
    /// and takes no lock; nothing changes.
    pub fn unconfirmed<E: From<OpLogError>>(
        &self,
        held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        match Marker::read(&self.durable_path).unsynced_from() {
            Some(from) if from < self.len_bytes => self.first_unheld(from, held),
            _ => Ok(None),
        }
    }

    /// For a writer, before it appends: reads the log to its end and cuts
    /// it from the first op on that starts past the synced boundary and
    /// that `held` does not vouch for; the marker moves to the next
    /// generation first. Returns what was cut. The handle then holds the
    /// log as it is now, read from its first record: state derived from
    /// its ops is to be derived again.
    ///
    /// `held` must see everything a live writer stored before it appended
    /// its op: the caller keeps the content store from being appended to,
    /// and has read it to its end, for as long as this runs.
    pub fn settle<E: From<OpLogError>>(
        &mut self,
        held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        lock_exclusive(&self.file).map_err(OpLogError::from)?;
        let settled = self.settle_locked(held);
        let _ = unlock(&self.file);
        settled
    }

    fn settle_locked<E: From<OpLogError>>(
        &mut self,
        held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        match self.catch_up() {
            Err(OpLogError::CutBack) => {
                self.reopen_locked()?;
                self.catch_up()?;
            }
            caught_up => caught_up?,
        }
        let marker = Marker::read(&self.durable_path);
        synced::lost(&marker, self.len_bytes)?;
        let Some(from) = marker.unsynced_from().filter(|from| *from < self.len_bytes) else {
            return Ok(None);
        };
        let Some(cut) = self.first_unheld(from, held)? else {
            return Ok(None);
        };
        let ops = self.ops()?;
        let discarded = crate::Discarded {
            from: cut.at,
            to: self.len_bytes,
            ops: ops[ops.len() - cut.ops..].to_vec(),
            unread: 0,
        };
        synced::cut_records(&self.durable_path, &self.file, cut.at).map_err(OpLogError::from)?;
        self.note_discarded(discarded);
        self.reopen_locked()?;
        Ok(Some(cut))
    }

    /// [`Self::reopen`] for a caller that holds the log's lock.
    fn reopen_locked(&mut self) -> Result<(), OpLogError> {
        let scan = scan(read_log(&self.file, None)?)?.unrepaired(&self.durable_path)?;
        self.base = None;
        self.ops = scan.ops;
        self.full = OnceLock::new();
        self.len_bytes = scan.valid_len.max(HEADER_LEN as u64);
        self.last_start = scan.last_start;
        self.others_to = self.len_bytes;
        self.reads += 1;
        Ok(())
    }
}
