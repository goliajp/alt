//! The synced marker `odb.durable`: how far each append file is known to
//! be on disk.
//!
//! It answers two questions. A writer about to fsync asks whether another
//! writer's fsync already covered its appends (group commit). An open that
//! finds a record it cannot read asks whether that record lies in the part
//! of the file that was synced — damage, reported — or past it — a write
//! that never finished, cut off (see [`alt_store::Tail`]).
//!
//! ```text
//! [0..24)    zero
//! [24..28)   magic "ALT2"
//! [28..32)   zero
//! [32..128)  slot 0
//! [128..224) slot 1
//! slot: [serial u64][generation u64][flags u32][pack seq u32]
//!       [eof: pack u64, blob map u64, tier1 u64, map.alt u64]
//!       [whole: pack u64, blob map u64, tier1 u64, map.alt u64]
//!       [checksum 8 = blake3(the 88 bytes before it)[..8]]
//! ```
//!
//! `whole` are record boundaries known synced. `eof` are file lengths at
//! the last fsync, which can include a dead writer's unfinished record:
//! they only tell whether an fsync can be skipped. `generation` moves with
//! every rewind, so a writer whose records a rewind cut off does not record
//! them afterwards.
//!
//! Every change is made in place under an exclusive lock on the marker
//! file itself ([`update`]): it goes to the slot that does not hold the
//! current state, with the next serial, and is fdatasynced. A reader takes
//! the valid slot with the larger serial, so a write a crash interrupted
//! leaves the state before it. The file never claims more than is on disk:
//! it is raised only after the fsync it records and lowered before a cut.
//!
//! An older alt keeps three file lengths in the first 24 bytes and replaces
//! the whole file; reading zeros there, it fsyncs every time.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use alt_store::Basis;

use crate::lock::lock_exclusive;

const MAGIC: [u8; 4] = *b"ALT2";
const LEGACY_LEN: usize = 24;
const SLOTS_AT: usize = 32;
const SLOT_LEN: usize = 96;
const LEN: usize = SLOTS_AT + 2 * SLOT_LEN;
/// Every append file starts with a five-byte header.
pub(crate) const HEADER: u64 = 5;
/// The flag of a marker past whose boundaries appends were left unsynced.
pub(crate) const UNTRUSTED: u32 = 1;

/// One of the four append files.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Log {
    Pack,
    Blobmap,
    Tier1,
    Map,
}

/// Lengths of the four append files; `seq` names the active pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Lens {
    pub seq: u32,
    pub pack: u64,
    pub blobmap: u64,
    pub tier1: u64,
    pub map: u64,
}

impl Lens {
    /// A store whose files were just created.
    pub const NEW: Lens = Lens {
        seq: 1,
        pack: HEADER,
        blobmap: HEADER,
        tier1: HEADER,
        map: HEADER,
    };

    const NONE: Lens = Lens {
        seq: 0,
        pack: 0,
        blobmap: 0,
        tier1: 0,
        map: 0,
    };

    /// The further of the two in every file. A later pack is further than
    /// any length of an earlier one, which was synced whole when sealed.
    pub fn max(self, other: Lens) -> Lens {
        let (seq, pack) = (self.seq, self.pack).max((other.seq, other.pack));
        Lens {
            seq,
            pack,
            blobmap: self.blobmap.max(other.blobmap),
            tier1: self.tier1.max(other.tier1),
            map: self.map.max(other.map),
        }
    }

    /// The nearer of the two in every file.
    pub fn min(self, other: Lens) -> Lens {
        let (seq, pack) = (self.seq, self.pack).min((other.seq, other.pack));
        Lens {
            seq,
            pack,
            blobmap: self.blobmap.min(other.blobmap),
            tier1: self.tier1.min(other.tier1),
            map: self.map.min(other.map),
        }
    }

    /// Whether these lengths are at or past `target` in every file.
    pub fn reaches(&self, target: &Lens) -> bool {
        (self.seq, self.pack) >= (target.seq, target.pack)
            && self.blobmap >= target.blobmap
            && self.tier1 >= target.tier1
            && self.map >= target.map
    }

    /// The length of `log`, of pack `seq` for the pack. A pack later than
    /// the one recorded was created since, synced with its header and no
    /// further; of an earlier one nothing is known.
    pub fn of(&self, log: Log, seq: u32) -> Option<u64> {
        match log {
            Log::Pack => match seq.cmp(&self.seq) {
                std::cmp::Ordering::Equal => Some(self.pack),
                std::cmp::Ordering::Greater => Some(HEADER),
                std::cmp::Ordering::Less => None,
            },
            Log::Blobmap => Some(self.blobmap),
            Log::Tier1 => Some(self.tier1),
            Log::Map => Some(self.map),
        }
    }

    /// Brings the length of `log` (pack `seq` for the pack) down to `at`;
    /// says whether it was past it.
    fn lower(&mut self, log: Log, seq: u32, at: u64) -> bool {
        let len = match log {
            Log::Pack if seq != self.seq => return false,
            Log::Pack => &mut self.pack,
            Log::Blobmap => &mut self.blobmap,
            Log::Tier1 => &mut self.tier1,
            Log::Map => &mut self.map,
        };
        let past = *len > at;
        *len = (*len).min(at);
        past
    }

    fn encode(&self, out: &mut Vec<u8>) {
        for len in [self.pack, self.blobmap, self.tier1, self.map] {
            out.extend_from_slice(&len.to_le_bytes());
        }
    }

    fn decode(seq: u32, data: &[u8]) -> Lens {
        let word = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        Lens {
            seq,
            pack: word(0),
            blobmap: word(8),
            tier1: word(16),
            map: word(24),
        }
    }
}

/// What a marker in this format records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct State {
    /// moved by every rewind
    pub generation: u64,
    /// bit 0: appends that were never fsynced may lie past `whole`
    pub flags: u32,
    /// file lengths at the last fsync, of the same pack as `whole`
    pub eof: Lens,
    /// record boundaries known synced
    pub whole: Lens,
}

impl State {
    /// A store whose files were just created.
    pub const NEW: State = State {
        generation: 0,
        flags: 0,
        eof: Lens::NEW,
        whole: Lens::NEW,
    };

    const NONE: State = State {
        generation: 0,
        flags: 0,
        eof: Lens::NONE,
        whole: Lens::NONE,
    };

    fn encode(&self, serial: u64) -> [u8; SLOT_LEN] {
        let mut slot = Vec::with_capacity(SLOT_LEN);
        slot.extend_from_slice(&serial.to_le_bytes());
        slot.extend_from_slice(&self.generation.to_le_bytes());
        slot.extend_from_slice(&self.flags.to_le_bytes());
        slot.extend_from_slice(&self.whole.seq.to_le_bytes());
        self.eof.encode(&mut slot);
        self.whole.encode(&mut slot);
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
        let half = |at: usize| u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
        let seq = half(20);
        let state = State {
            generation: word(8),
            flags: half(16),
            eof: Lens::decode(seq, &body[24..56]),
            whole: Lens::decode(seq, &body[56..88]),
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
    /// the file lengths an older alt recorded at its last fsync: active
    /// pack, blob map, `map.alt`
    Legacy([u64; 3]),
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

    /// The generation a writer compares later: a marker that records none
    /// is at zero, and the rewind that first writes one starts past it.
    pub fn generation(&self) -> u64 {
        self.recorded().map_or(0, |state| state.generation)
    }

    /// Whether the marker records boundaries that every append since was
    /// synced before it moved them.
    pub fn trusted(&self) -> bool {
        self.recorded()
            .is_some_and(|state| state.flags & UNTRUSTED == 0)
    }

    /// What the marker tells about `log` (pack `seq` for the pack, `len`
    /// bytes long) not reading to its end. `bounded`: whether the store
    /// has had a marker in this format, asked when there is none.
    pub fn basis(&self, log: Log, seq: u32, len: u64, bounded: impl Fn() -> bool) -> Basis {
        match self {
            Marker::Recorded(state) if state.flags & UNTRUSTED != 0 => Basis::LastOnly,
            Marker::Recorded(state) => state
                .whole
                .of(log, seq)
                .map_or(Basis::Unknown, Basis::Boundary),
            Marker::Legacy([pack, blobmap, map]) => {
                let eof = match log {
                    // a length past the end of the pack is of one sealed since
                    Log::Pack if *pack <= len => *pack,
                    Log::Blobmap => *blobmap,
                    Log::Map => *map,
                    // an older alt records nothing of the tier1 map
                    Log::Pack | Log::Tier1 => 0,
                };
                // a length short of the file header is not one an older alt
                // recorded: the marker is being replaced as it is read
                match eof >= HEADER {
                    true => Basis::Boundary(eof),
                    false => Basis::LastOnly,
                }
            }
            Marker::Unreadable => Basis::LastOnly,
            Marker::Absent if bounded() => Basis::Unknown,
            Marker::Absent => Basis::LastOnly,
        }
    }
}

/// What `data` says and, for a marker in this format, the slot the next
/// write goes to with its serial.
fn parse(data: &[u8]) -> (Marker, Option<(usize, u64)>) {
    if data.len() < LEGACY_LEN {
        return (Marker::Unreadable, None);
    }
    if data.get(LEGACY_LEN..LEGACY_LEN + 4) != Some(&MAGIC[..]) {
        let word = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        return (Marker::Legacy([word(0), word(8), word(16)]), None);
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

/// Records that the records up to `whole` are synced, for a writer that
/// read `generation` while it held the write lock, and with them the file
/// lengths `eof` of an fsync it made itself. Nothing is recorded when a
/// rewind has moved the generation since: it cut those records off.
pub(crate) fn record(
    path: &Path,
    generation: u64,
    whole: Lens,
    eof: Option<Lens>,
) -> std::io::Result<()> {
    update(path, |marker| {
        let mut state = marker.recorded().copied().unwrap_or(State::NONE);
        if state.generation != generation {
            return None;
        }
        state.whole = state.whole.max(whole);
        if let Some(eof) = eof {
            state.eof = state.eof.max(eof);
        }
        if state.eof.seq < state.whole.seq {
            // nothing is known of the lengths this pack was fsynced at
            state.eof = Lens {
                seq: state.whole.seq,
                pack: 0,
                ..state.eof
            };
        }
        Some(state)
    })
}

/// Called before `log` (pack `seq` for the pack) is cut back to `at`: a
/// file length recorded past the cut comes down first, so that what grows
/// back there is not taken for bytes an fsync covered.
pub(crate) fn lower_eof(path: &Path, log: Log, seq: u32, at: u64) -> std::io::Result<()> {
    update(path, |marker| {
        let mut state = *marker.recorded()?;
        state.eof.lower(log, seq, at).then_some(state)
    })
}

/// Makes `lens` the boundaries of a store whose marker recorded none that
/// hold: the caller holds the write lock, has read every file to its end
/// and has fsynced them.
pub(crate) fn establish(path: &Path, lens: Lens) -> std::io::Result<()> {
    update(path, |marker| {
        Some(State {
            generation: marker.generation(),
            flags: 0,
            eof: lens,
            whole: lens,
        })
    })
}

/// Marks the boundaries as no longer telling an unfinished append from
/// damage: appends that are not synced are about to follow them.
pub(crate) fn distrust(path: &Path) -> std::io::Result<()> {
    update(path, |marker| {
        let state = marker.recorded().copied().unwrap_or(State::NONE);
        Some(State {
            flags: state.flags | UNTRUSTED,
            ..state
        })
    })
}

/// What a handle whose fsyncs are left to a sink (the daemon) last finished
/// writing, for that sink to record once it has synced: the boundaries and
/// the marker's generation when they were written.
pub(crate) type Deferred = Arc<Mutex<Option<(Lens, u64)>>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn lens(n: u64) -> Lens {
        Lens {
            seq: 1,
            pack: n,
            blobmap: n,
            tier1: n,
            map: n,
        }
    }

    fn state(n: u64) -> State {
        State {
            generation: 0,
            flags: 0,
            eof: lens(n),
            whole: lens(n),
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
        let path = dir.path().join("odb.durable");
        set(&path, 10);
        let data = std::fs::read(&path).unwrap();
        assert_eq!(data.len(), LEN);
        assert_eq!(
            data[..LEGACY_LEN],
            [0u8; LEGACY_LEN],
            "zeros for an older alt"
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
        let path = dir.path().join("odb.durable");
        for n in 10..13 {
            set(&path, n);
        }
        assert_eq!(serials(&path), [Some(3), Some(2)]);
        // the write of serial 3 torn: the state before it stands
        flip(&path, SLOTS_AT + 40);
        assert_eq!(Marker::read(&path), Marker::Recorded(state(11)));
        // and the next write goes to the broken slot
        set(&path, 20);
        assert_eq!(serials(&path), [Some(3), Some(2)]);
        assert_eq!(Marker::read(&path), Marker::Recorded(state(20)));

        flip(&path, SLOTS_AT + 40);
        flip(&path, SLOTS_AT + SLOT_LEN + 40);
        assert_eq!(Marker::read(&path), Marker::Unreadable);
        set(&path, 30);
        assert_eq!(serials(&path), [Some(1), None]);
        assert_eq!(Marker::read(&path), Marker::Recorded(state(30)));
    }

    #[test]
    fn the_slot_with_the_larger_serial_is_the_state() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("odb.durable");
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
        let path = dir.path().join("odb.durable");
        assert_eq!(Marker::read(&path), Marker::Absent);
        // an update with nothing to write does not create it
        update(&path, |_| None).unwrap();
        assert!(!path.exists());

        let older: Vec<u8> = [7u64, 8, 9].iter().flat_map(|n| n.to_le_bytes()).collect();
        for extra in [
            &b""[..],
            &b"ALTD and what an unreleased build put after it"[..],
        ] {
            std::fs::write(&path, [&older[..], extra].concat()).unwrap();
            assert_eq!(Marker::read(&path), Marker::Legacy([7, 8, 9]));
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
        std::fs::write(&path, &older).unwrap();
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
        let path = dir.path().join("odb.durable");
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..EACH {
                        let count = |marker: &Marker| {
                            let mut state = marker.recorded().copied().unwrap_or(State::NEW);
                            state.eof.map += 1;
                            Some(state)
                        };
                        update(&path, count).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let Marker::Recorded(state) = Marker::read(&path) else {
            panic!("no state recorded");
        };
        assert_eq!(state.eof.map, HEADER + THREADS * EACH);
    }

    #[test]
    fn nothing_is_recorded_for_a_writer_from_before_a_rewind() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("odb.durable");
        record(&path, 0, lens(10), Some(lens(10))).unwrap();
        let rewound = State {
            generation: 1,
            ..state(10)
        };
        update(&path, |_| Some(rewound)).unwrap();
        record(&path, 0, lens(20), Some(lens(20))).unwrap();
        assert_eq!(Marker::read(&path), Marker::Recorded(rewound));
        // a writer since records its boundaries, and no file length without
        // an fsync of its own
        record(&path, 1, lens(20), None).unwrap();
        let expected = State {
            whole: lens(20),
            ..rewound
        };
        assert_eq!(Marker::read(&path), Marker::Recorded(expected));
    }

    #[test]
    fn a_cut_lowers_the_file_length_of_that_log_and_nothing_else() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("odb.durable");
        // nothing recorded: nothing to lower, and no marker is made up
        lower_eof(&path, Log::Map, 0, 3).unwrap();
        assert_eq!(Marker::read(&path), Marker::Absent);
        set(&path, 100);
        // another pack than the one recorded, and a cut past the length
        lower_eof(&path, Log::Pack, 2, 40).unwrap();
        lower_eof(&path, Log::Tier1, 0, 100).unwrap();
        assert_eq!(serials(&path), [Some(1), None]);
        lower_eof(&path, Log::Pack, 1, 40).unwrap();
        lower_eof(&path, Log::Map, 0, 60).unwrap();
        let mut expected = state(100);
        expected.eof.pack = 40;
        expected.eof.map = 60;
        assert_eq!(Marker::read(&path), Marker::Recorded(expected));
    }
}
