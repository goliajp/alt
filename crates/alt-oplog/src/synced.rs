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
//! ```
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
//! then missing it was removed, not never written.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::OpLogError;
use crate::lock::{lock_exclusive, unlock};

const MAGIC: [u8; 4] = *b"ALT2";
const LEGACY_LEN: usize = 8;
const SLOTS_AT: usize = 16;
const SLOT_LEN: usize = 36;
const LEN: usize = SLOTS_AT + 2 * SLOT_LEN;
/// Every log starts with a five-byte header.
const HEADER: u64 = 5;
/// The flag of a marker past whose boundary appends were left unsynced.
const UNTRUSTED: u32 = 1;

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
    fn encode(&self, serial: u64) -> [u8; SLOT_LEN] {
        let mut slot = Vec::with_capacity(SLOT_LEN);
        slot.extend_from_slice(&serial.to_le_bytes());
        slot.extend_from_slice(&self.flags.to_le_bytes());
        slot.extend_from_slice(&self.eof.to_le_bytes());
        slot.extend_from_slice(&self.whole.to_le_bytes());
        let sum = check(&slot);
        slot.extend_from_slice(&sum);
        slot.try_into().unwrap()
    }

    /// The serial and state of a slot whose checksum holds.
    fn decode(slot: &[u8]) -> Option<(u64, State)> {
        let (body, sum) = slot.split_at(SLOT_LEN - 8);
        if check(body) != *sum {
            return None;
        }
        let word = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
        let state = State {
            flags: u32::from_le_bytes(body[8..12].try_into().unwrap()),
            eof: word(12),
            whole: word(20),
        };
        Some((word(0), state))
    }
}

fn check(body: &[u8]) -> [u8; 8] {
    alt_hash::blake3::hash(body).as_bytes()[..8]
        .try_into()
        .unwrap()
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
    /// Reads the marker, taking no lock.
    pub fn read(path: &Path) -> Marker {
        match std::fs::read(path) {
            Ok(data) => parse(&data).0,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Marker::Absent,
            Err(_) => Marker::Unreadable,
        }
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

    /// Whether the marker records a boundary that every append since was
    /// synced before it moved it.
    pub fn trusted(&self) -> bool {
        self.recorded()
            .is_some_and(|state| state.flags & UNTRUSTED == 0)
    }

    /// Whether a record that does not read at `at` is an append that never
    /// finished rather than damage; `last`: no whole record can follow it.
    /// Past a boundary that holds it never finished, and before it is
    /// damage. The file length an older alt recorded is such a boundary.
    /// Where nothing is recorded, or unsynced appends followed, only the
    /// last record is taken for unfinished. A marker that was removed
    /// tells nothing: every such record is damage.
    pub fn unfinished(path: &Path, at: u64, last: bool) -> bool {
        match Self::read(path) {
            Marker::Recorded(state) if state.flags & UNTRUSTED == 0 => at >= state.whole,
            // a length short of the header is not one an older alt
            // recorded: the marker is being replaced as it is read
            Marker::Legacy(eof) if eof >= HEADER => at >= eof,
            Marker::Recorded(_) | Marker::Legacy(_) | Marker::Unreadable => last,
            Marker::Absent => last && !bounded(path),
        }
    }
}

/// Whether the log beside the marker at `path` has had a marker in this
/// format.
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

/// Makes `len` the boundary of a log whose marker recorded none that
/// holds: the caller holds the log's lock, has read it to its end and has
/// fsynced it.
pub(crate) fn establish(path: &Path, len: u64) -> std::io::Result<()> {
    update(path, |_| {
        Some(State {
            flags: 0,
            eof: len,
            whole: len,
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

/// What `data` says and, for a marker in this format, the slot the next
/// write goes to with its serial.
fn parse(data: &[u8]) -> (Marker, Option<(usize, u64)>) {
    if data.len() < LEGACY_LEN {
        return (Marker::Unreadable, None);
    }
    if data.get(LEGACY_LEN..LEGACY_LEN + 4) != Some(&MAGIC[..]) {
        let eof = u64::from_le_bytes(data[..8].try_into().unwrap());
        return (Marker::Legacy(eof), None);
    }
    let slot = |n: usize| {
        let at = SLOTS_AT + n * SLOT_LEN;
        data.get(at..at + SLOT_LEN).and_then(State::decode)
    };
    match (slot(0), slot(1)) {
        (None, None) => (Marker::Unreadable, None),
        (Some((serial, state)), None) => (Marker::Recorded(state), Some((1, serial + 1))),
        (None, Some((serial, state))) => (Marker::Recorded(state), Some((0, serial + 1))),
        (Some((first, state)), Some((second, _))) if first > second => {
            (Marker::Recorded(state), Some((1, first + 1)))
        }
        (Some(_), Some((second, state))) => (Marker::Recorded(state), Some((0, second + 1))),
    }
}

/// A whole marker file holding `state`, for a file that is not one in this
/// format: it is replaced in one write.
fn fresh(state: &State) -> Vec<u8> {
    let mut file = vec![0u8; LEN];
    file[LEGACY_LEN..LEGACY_LEN + 4].copy_from_slice(&MAGIC);
    file[SLOTS_AT..SLOTS_AT + SLOT_LEN].copy_from_slice(&state.encode(1));
    file
}

/// Changes the marker: `next` is given what it says and returns what it
/// is to say, or `None` to leave it as it is. Reading, deciding and writing
/// happen under an exclusive lock on the marker file, taken through a
/// descriptor opened here, so that threads of one process exclude each
/// other like processes do; nothing else is locked while it is held. The
/// new state is fdatasynced before this returns. A marker that is absent
/// is created only when there is something to write.
pub(crate) fn update(path: &Path, next: impl Fn(&Marker) -> Option<State>) -> std::io::Result<()> {
    let open = |create: bool| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .open(path)
    };
    let (mut file, created) = match open(false) {
        Ok(file) => (file, false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if next(&Marker::Absent).is_none() {
                return Ok(());
            }
            (open(true)?, true)
        }
        Err(e) => return Err(e),
    };
    lock_exclusive(&file)?;
    let mut data = Vec::with_capacity(LEN);
    file.read_to_end(&mut data)?;
    let (marker, slot) = match created && data.is_empty() {
        true => (Marker::Absent, None),
        false => parse(&data),
    };
    let Some(state) = next(&marker) else {
        return Ok(());
    };
    if marker == Marker::Recorded(state) {
        return Ok(());
    }
    let (at, bytes) = match slot {
        Some((n, serial)) => (SLOTS_AT + n * SLOT_LEN, state.encode(serial).to_vec()),
        None => (0, fresh(&state)),
    };
    file.seek(SeekFrom::Start(at as u64))?;
    file.write_all(&bytes)?;
    file.sync_data()?;
    if let (true, Some(dir)) = (created, path.parent()) {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// Records that the records up to `whole` are synced and, for a committer
/// that fsynced itself, the file length `eof` it did so at.
pub(crate) fn record(path: &Path, whole: u64, eof: Option<u64>) -> std::io::Result<()> {
    update(path, |marker| {
        let state = marker.recorded().copied().unwrap_or_default();
        Some(State {
            eof: state.eof.max(eof.unwrap_or(0)),
            whole: state.whole.max(whole),
            ..state
        })
    })
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

pub(crate) fn open_sync_lock(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
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
        if target != 0 {
            // a boundary this log wrote, not a file length: nothing cuts it
            record(&self.durable_path, target, Some(target))?;
        }
        Ok(())
    }
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

    fn serials(path: &Path) -> [Option<u64>; 2] {
        let data = std::fs::read(path).unwrap();
        [0, 1].map(|n| {
            let at = SLOTS_AT + n * SLOT_LEN;
            let slot = data.get(at..at + SLOT_LEN)?;
            State::decode(slot).map(|(serial, _)| serial)
        })
    }

    fn flip(path: &Path, at: usize) {
        let mut data = std::fs::read(path).unwrap();
        data[at] ^= 0xff;
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn writes_take_turns_between_the_two_slots() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("durable");
        set(&path, 10);
        let data = std::fs::read(&path).unwrap();
        assert_eq!(data.len(), LEN);
        assert_eq!(
            data[..LEGACY_LEN],
            [0u8; LEGACY_LEN],
            "zero for an older alt"
        );
        assert_eq!(serials(&path), [Some(1), None]);
        let mut turns = Vec::new();
        for n in 11..15 {
            set(&path, n);
            assert_eq!(Marker::read(&path), Marker::Recorded(state(n)));
            turns.push(serials(&path));
        }
        let expected = [
            [Some(1), Some(2)],
            [Some(3), Some(2)],
            [Some(3), Some(4)],
            [Some(5), Some(4)],
        ];
        assert_eq!(turns, expected);
        // writing what is there already writes nothing
        set(&path, 14);
        assert_eq!(serials(&path), [Some(5), Some(4)]);
    }

    #[test]
    fn a_slot_that_does_not_check_gives_way_to_the_other() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("durable");
        for n in 10..13 {
            set(&path, n);
        }
        assert_eq!(serials(&path), [Some(3), Some(2)]);
        // the write of serial 3 torn: the state before it stands
        flip(&path, SLOTS_AT + 20);
        assert_eq!(Marker::read(&path), Marker::Recorded(state(11)));
        // and the next write goes to the broken slot
        set(&path, 20);
        assert_eq!(serials(&path), [Some(3), Some(2)]);
        assert_eq!(Marker::read(&path), Marker::Recorded(state(20)));

        flip(&path, SLOTS_AT + 20);
        flip(&path, SLOTS_AT + SLOT_LEN + 20);
        assert_eq!(Marker::read(&path), Marker::Unreadable);
        set(&path, 30);
        assert_eq!(serials(&path), [Some(1), None]);
        assert_eq!(Marker::read(&path), Marker::Recorded(state(30)));
    }

    #[test]
    fn the_slot_with_the_larger_serial_is_the_state() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("durable");
        for (first, second, current) in [(9, 7, 5), (7, 9, 50)] {
            let mut data = vec![0u8; SLOTS_AT];
            data[LEGACY_LEN..LEGACY_LEN + 4].copy_from_slice(&MAGIC);
            data.extend_from_slice(&state(5).encode(first));
            data.extend_from_slice(&state(50).encode(second));
            std::fs::write(&path, data).unwrap();
            assert_eq!(Marker::read(&path), Marker::Recorded(state(current)));
        }
    }

    #[test]
    fn an_older_marker_is_told_from_a_broken_one_and_from_none() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("durable");
        assert_eq!(Marker::read(&path), Marker::Absent);
        // an update with nothing to write does not create it
        update(&path, |_| None).unwrap();
        assert!(!path.exists());

        let older = 7u64.to_le_bytes();
        for extra in [
            &b""[..],
            &b"ALTD and what an unreleased build put after it"[..],
        ] {
            std::fs::write(&path, [&older[..], extra].concat()).unwrap();
            assert_eq!(Marker::read(&path), Marker::Legacy(7));
        }
        for short in [0, 1, LEGACY_LEN - 1] {
            std::fs::write(&path, vec![0u8; short]).unwrap();
            assert_eq!(Marker::read(&path), Marker::Unreadable, "{short} bytes");
        }
        let mut cut = vec![0u8; SLOTS_AT];
        cut[LEGACY_LEN..LEGACY_LEN + 4].copy_from_slice(&MAGIC);
        std::fs::write(&path, cut).unwrap();
        assert_eq!(Marker::read(&path), Marker::Unreadable, "no slot");

        // an older marker is replaced whole
        std::fs::write(&path, older).unwrap();
        set(&path, 10);
        assert_eq!(Marker::read(&path), Marker::Recorded(state(10)));
        let data = std::fs::read(&path).unwrap();
        assert_eq!(
            (data.len(), &data[..LEGACY_LEN]),
            (LEN, &[0u8; LEGACY_LEN][..])
        );
    }

    #[test]
    fn updates_from_many_threads_are_all_kept() {
        const THREADS: u64 = 8;
        const EACH: u64 = 100;
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("durable");
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..EACH {
                        let count = |marker: &Marker| {
                            let state = marker.recorded().copied().unwrap_or_default();
                            Some(State {
                                eof: state.eof + 1,
                                ..state
                            })
                        };
                        update(&path, count).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            Marker::read(&path).recorded().map(|state| state.eof),
            Some(THREADS * EACH)
        );
    }

    #[test]
    fn a_boundary_is_recorded_without_a_file_length_it_was_not_synced_at() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("durable");
        record(&path, 10, Some(40)).unwrap();
        record(&path, 30, None).unwrap();
        record(&path, 20, Some(35)).unwrap();
        let expected = State {
            flags: 0,
            eof: 40,
            whole: 30,
        };
        assert_eq!(Marker::read(&path), Marker::Recorded(expected));
    }
}
