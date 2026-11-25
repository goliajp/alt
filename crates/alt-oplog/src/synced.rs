//! The synced marker `durable`: how far the log is known to be on disk.
//!
//! A committer about to fsync asks it whether another committer's fsync
//! already covered its appends. An open that finds a record it cannot read
//! asks it whether that record lies in the synced part of the log — damage,
//! reported with the file left as it is — or past it — an append that never
//! finished. What it says when it records no such boundary is in
//! [`Marker::unfinished`].
//!
//! ```text
//! [0..8)   zero
//! [8..12)  magic "ALT2"
//! [12..16) zero
//! [16..52) slot 0
//! [52..88) slot 1
//! slot: [serial u64][flags u32][eof u64][whole u64]
//!       [checksum 8 = blake3(the 28 bytes before it)[..8]]
//! flags: bit 0, unsynced appends may follow `whole`; bits 8 and up, the
//!        generation
//! ```
//!
//! The generation goes up whenever whole records are cut from the log
//! (see [`crate::OpLog::settle`]). A committer notes it when it appends
//! and records nothing as synced under another one: what it appended may
//! be gone, and other records in its place.
//!
//! `whole` is a record boundary known synced. `eof` is the file length at
//! the last fsync, which can include a dead writer's unfinished record: it
//! only tells whether an fsync can be skipped.
//!
//! Every change is made in place under an exclusive lock on the marker
//! file itself ([`update`]): it goes to the slot that does not hold the
//! current state, with the next serial, and is fdatasynced. A reader takes
//! the valid slot with the larger serial, so a write a crash interrupted
//! leaves the state before it. The file never claims more than is on disk:
//! it is raised only after the fsync it records and lowered before a cut.
//!
//! An older alt keeps the file length in the first 8 bytes and replaces the
//! whole file; reading zero there, it fsyncs every time.
//!
//! The first byte of `sync.lock`, which an older alt only locks, says in
//! bit 0 that the log has had a marker in this format: when the marker is
//! then missing or does not read, a boundary was lost, not never written.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use crate::OpLogError;
use crate::left::open_sync_lock;
use crate::lock::{lock_exclusive, unlock};

/// Every log starts with a five-byte header.
const HEADER: u64 = 5;
/// The flag of a marker past whose boundary appends were left unsynced.
const UNTRUSTED: u32 = 1;
/// Where the generation starts in the flags.
const GENERATION: u32 = 8;

/// Syncs what the ops in the log name (see
/// [`crate::OpLog::sync_named_with`]).
pub type SyncNamed = Arc<dyn Fn(&[crate::Op]) -> std::io::Result<()> + Send + Sync>;

/// Before the marker is moved past `others_to`, where the ops of other
/// writers end: when it does not record them as synced yet, what they name
/// is synced first.
pub(crate) fn sync_named(
    named: &Option<SyncNamed>,
    marker: &Marker,
    others_to: u64,
) -> std::io::Result<()> {
    match named {
        Some(sync) if !marker.records(others_to) => sync(&[]),
        _ => Ok(()),
    }
}

/// What a marker in this format records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct State {
    /// bit 0: appends that were never fsynced may lie past `whole`
    pub flags: u32,
    /// file length at the last fsync
    pub eof: u64,
    /// record boundary known synced
    pub whole: u64,
}

impl State {
    pub fn generation(&self) -> u32 {
        self.flags >> GENERATION
    }
}

impl alt_marker::Slot for State {
    const LEGACY_LEN: usize = 8;
    const BODY_LEN: usize = 20;

    fn encode(&self, body: &mut [u8]) {
        body[..4].copy_from_slice(&self.flags.to_le_bytes());
        body[4..12].copy_from_slice(&self.eof.to_le_bytes());
        body[12..].copy_from_slice(&self.whole.to_le_bytes());
    }

    fn decode(body: &[u8]) -> Self {
        let word = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
        State {
            flags: u32::from_le_bytes(body[..4].try_into().unwrap()),
            eof: word(4),
            whole: word(12),
        }
    }
}

/// What the marker file says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Marker {
    Recorded(State),
    /// the file length an older alt recorded at its last fsync
    Legacy(u64),
    /// shorter than any marker, or no slot of it holds
    Unreadable,
    Absent,
}

impl Marker {
    /// Reads the marker, taking no lock: one read of as much of the file
    /// as a marker takes up.
    pub fn read(path: &Path) -> Marker {
        alt_marker::read::<State>(path).into()
    }

    pub fn recorded(&self) -> Option<&State> {
        match self {
            Marker::Recorded(state) => Some(state),
            _ => None,
        }
    }

    /// Whether the marker says the records up to `target` are synced.
    pub fn records(&self, target: u64) -> bool {
        self.recorded().is_some_and(|state| state.whole >= target)
    }

    /// Zero for a marker that records none.
    pub fn generation(&self) -> u32 {
        self.recorded().map_or(0, State::generation)
    }

    /// From which byte on the records of the log are not known to have
    /// been synced; `None` where the marker has no word that holds.
    pub fn unsynced_from(&self) -> Option<u64> {
        match self {
            Marker::Recorded(state) if self.trusted() => Some(state.whole),
            Marker::Legacy(eof) if *eof >= HEADER => Some(*eof),
            _ => None,
        }
    }

    /// Whether the marker records a boundary that every append since was
    /// synced before it moved it.
    pub fn trusted(&self) -> bool {
        self.recorded()
            .is_some_and(|state| state.flags & UNTRUSTED == 0)
    }

    /// Whether a record that does not read at `at` is an append that never
    /// finished rather than damage; `last`: no whole record can follow it.
    /// Past the boundary a marker records nothing was ever synced, whether
    /// or not appends were left unsynced there: it never finished, and
    /// before the boundary it is damage. The file length an older alt
    /// recorded is such a boundary. Where nothing is recorded only the
    /// last record is taken for unfinished, and a marker that is gone or
    /// does not read, in a log that has had one, makes every such record
    /// damage.
    pub fn unfinished(path: &Path, at: u64, last: bool) -> bool {
        match Self::read(path) {
            Marker::Recorded(state) => at >= state.whole,
            // a length short of the header is not one an older alt
            // recorded: the marker is being replaced as it is read
            Marker::Legacy(eof) if eof >= HEADER => at >= eof,
            Marker::Legacy(_) => last,
            Marker::Unreadable | Marker::Absent => last && !bounded(path),
        }
    }
}

/// Whether the log beside the marker at `path` has had one in this format.
fn bounded(path: &Path) -> bool {
    std::fs::read(path.with_file_name("sync.lock"))
        .is_ok_and(|data| data.first().is_some_and(|flags| flags & 1 != 0))
}

/// Notes that the log beside the marker at `path` has a marker in this
/// format from now on.
pub(crate) fn note_bounded(path: &Path) -> std::io::Result<()> {
    if bounded(path) {
        return Ok(());
    }
    let mut file = open_sync_lock(&path.with_file_name("sync.lock"))?;
    file.write_all(&[1])?;
    file.sync_all()
}

/// Fails when a log that holds whole records up to `holds` is short of
/// the records the marker says were synced.
pub(crate) fn lost(marker: &Marker, holds: u64) -> Result<(), OpLogError> {
    match marker.recorded() {
        Some(state) if holds < state.whole => Err(OpLogError::Lost {
            synced: state.whole,
            holds,
        }),
        _ => Ok(()),
    }
}

impl crate::v1::OpLog {
    /// Whether the log still holds the records its marker says were
    /// synced; [`OpLogError::Lost`] when it does not. Reads the marker and
    /// takes no lock: an append checks the same under the lock.
    pub fn check_synced(&self) -> Result<(), OpLogError> {
        lost(&Marker::read(&self.durable_path), self.len_bytes)
    }
}

/// Makes `len` the boundary of a log whose marker recorded none that
/// holds, unless one past `len` was recorded meanwhile: the caller holds
/// the log's lock, has read it to its end and has fsynced it.
pub(crate) fn establish(path: &Path, len: u64) -> std::io::Result<()> {
    update(path, |marker| {
        let state = marker.recorded().copied().unwrap_or_default();
        Some(State {
            flags: state.generation() << GENERATION,
            eof: state.eof.max(len),
            whole: state.whole.max(len),
        })
    })?;
    note_bounded(path)
}

/// Marks the boundary as no longer telling an unfinished append from
/// damage: appends that are not synced are about to follow it.
pub(crate) fn distrust(path: &Path) -> std::io::Result<()> {
    update(path, |marker| {
        let state = marker.recorded().copied().unwrap_or_default();
        Some(State {
            flags: state.flags | UNTRUSTED,
            ..state
        })
    })
}

impl From<alt_marker::Marker<State>> for Marker {
    fn from(read: alt_marker::Marker<State>) -> Marker {
        match read {
            alt_marker::Marker::Recorded(state) => Marker::Recorded(state),
            alt_marker::Marker::Legacy(head) => {
                Marker::Legacy(u64::from_le_bytes(head[..8].try_into().unwrap()))
            }
            alt_marker::Marker::Unreadable => Marker::Unreadable,
            alt_marker::Marker::Absent => Marker::Absent,
        }
    }
}

/// Changes the marker: `next` is given what it says and returns what it
/// is to say, or `None` to leave it as it is ([`alt_marker::update`]:
/// under an exclusive lock on the marker file, nothing else locked while
/// it is held, and fdatasynced before this returns).
pub(crate) fn update(path: &Path, next: impl Fn(&Marker) -> Option<State>) -> std::io::Result<()> {
    alt_marker::update(path, |read: &alt_marker::Marker<State>| {
        next(&read.clone().into())
    })
}

/// Records that the records up to `whole` are synced and, for a committer
/// that fsynced itself, the file length `eof` it did so at. Nothing is
/// recorded under a generation other than the one the committer appended
/// under. Returns the boundary the marker then holds, when it holds this
/// one.
pub(crate) fn record(
    path: &Path,
    generation: u32,
    whole: u64,
    eof: Option<u64>,
) -> std::io::Result<Option<u64>> {
    let held = std::cell::Cell::new(None);
    update(path, |marker| {
        let state = marker.recorded().copied().unwrap_or_default();
        if state.generation() != generation {
            return None;
        }
        let whole = state.whole.max(whole);
        held.set(Some(whole));
        Some(State {
            eof: state.eof.max(eof.unwrap_or(0)),
            whole,
            ..state
        })
    })?;
    Ok(held.get())
}

/// Cuts the log `file` back to `at`; the caller holds its exclusive lock.
/// A file length recorded past the cut comes down first, so that what
/// grows back there is not taken for bytes an fsync covered. The sync lock
/// is held throughout: a committer that measured the file before the cut
/// has recorded that length before it is brought down, not after.
pub(crate) fn cut(durable: &Path, file: &File, at: u64) -> std::io::Result<()> {
    let sync_lock = open_sync_lock(&durable.with_file_name("sync.lock"))?;
    lock_exclusive(&sync_lock)?;
    let cut = (|| {
        update(durable, |marker| {
            let state = marker.recorded()?;
            (state.eof > at).then_some(State { eof: at, ..*state })
        })?;
        file.set_len(at)?;
        file.sync_all()
    })();
    let _ = unlock(&sync_lock);
    cut
}

/// Cuts the whole records from `at` on off the log `file`, under its
/// exclusive lock, and moves the marker to the next generation. An older
/// alt's marker, whose length `at` does not come before, gives way to one
/// that records that length as the boundary.
pub(crate) fn cut_records(durable: &Path, file: &File, at: u64) -> std::io::Result<()> {
    let sync_lock = open_sync_lock(&durable.with_file_name("sync.lock"))?;
    lock_exclusive(&sync_lock)?;
    let cut = (|| {
        update(durable, |marker| {
            let state = match marker {
                Marker::Recorded(state) => *state,
                Marker::Legacy(eof) => State {
                    flags: 0,
                    eof: *eof,
                    whole: *eof,
                },
                _ => return None,
            };
            Some(State {
                flags: (state.flags & UNTRUSTED) | (state.generation() + 1) << GENERATION,
                eof: state.eof.min(at),
                whole: state.whole,
            })
        })?;
        note_bounded(durable)?;
        file.set_len(at)?;
        file.sync_all()
    })();
    let _ = unlock(&sync_lock);
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(n: u64) -> State {
        State {
            flags: 0,
            eof: n,
            whole: n,
        }
    }

    fn set(path: &Path, n: u64) {
        update(path, |_| Some(state(n))).unwrap();
    }

    #[test]
    fn a_boundary_is_recorded_without_a_file_length_it_was_not_synced_at() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("durable");
        record(&path, 0, 10, Some(40)).unwrap();
        record(&path, 0, 30, None).unwrap();
        record(&path, 0, 20, Some(35)).unwrap();
        // another generation's committer records nothing
        record(&path, 1, 90, Some(90)).unwrap();
        let expected = State {
            flags: 0,
            eof: 40,
            whole: 30,
        };
        assert_eq!(Marker::read(&path), Marker::Recorded(expected));
    }

    #[test]
    fn an_older_marker_gives_the_length_it_recorded() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("durable");
        assert_eq!(Marker::read(&path), Marker::Absent);
        std::fs::write(&path, 9u64.to_le_bytes()).unwrap();
        assert_eq!(Marker::read(&path), Marker::Legacy(9));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(Marker::read(&path), Marker::Unreadable);
        // an older marker is replaced whole
        std::fs::write(&path, 9u64.to_le_bytes()).unwrap();
        set(&path, 10);
        assert_eq!(Marker::read(&path), Marker::Recorded(state(10)));
        let data = std::fs::read(&path).unwrap();
        assert_eq!((data.len(), &data[..8]), (88, &[0u8; 8][..]));
    }
}
