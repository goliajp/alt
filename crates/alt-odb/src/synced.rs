//! The synced marker `odb.durable`: how far each append file is known to
//! be on disk.
//!
//! It answers two questions. A writer about to fsync asks whether another
//! writer's fsync already covered its appends (group commit). An open that
//! finds a record it cannot read asks whether that record lies in the part
//! of the file that was synced — damage, reported — or past it — a write
//! that never finished, cut off (see [`alt_store::Tail`]).
//!
//! It is a marker file of two slots ([`alt_marker`]) behind 24 bytes of
//! zeros; a slot holds
//!
//! ```text
//! [generation u64][flags u32][pack seq u32]
//! [eof: pack u64, blob map u64, tier1 u64, map.alt u64]
//! [whole: pack u64, blob map u64, tier1 u64, map.alt u64]
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

use std::cell::Cell;
use std::path::Path;

use alt_store::Basis;
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

    pub(crate) const NONE: Lens = Lens {
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

    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        for len in [self.pack, self.blobmap, self.tier1, self.map] {
            out.extend_from_slice(&len.to_le_bytes());
        }
    }

    pub(crate) fn decode(seq: u32, data: &[u8]) -> Lens {
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
}

impl alt_marker::Slot for State {
    const LEGACY_LEN: usize = 24;
    const BODY_LEN: usize = 80;

    fn encode(&self, body: &mut [u8]) {
        let mut out = Vec::with_capacity(Self::BODY_LEN);
        out.extend_from_slice(&self.generation.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.whole.seq.to_le_bytes());
        self.eof.encode(&mut out);
        self.whole.encode(&mut out);
        body.copy_from_slice(&out);
    }

    fn decode(body: &[u8]) -> Self {
        let word = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
        let half = |at: usize| u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
        let seq = half(12);
        State {
            generation: word(0),
            flags: half(8),
            eof: Lens::decode(seq, &body[16..48]),
            whole: Lens::decode(seq, &body[48..80]),
        }
    }
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
    /// has had a marker in this format, asked when there is none or it
    /// does not read: boundaries were then lost, not never written.
    pub fn basis(&self, log: Log, seq: u32, len: u64, bounded: impl Fn() -> bool) -> Basis {
        match self {
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
            Marker::Unreadable | Marker::Absent if bounded() => Basis::Unknown,
            Marker::Unreadable | Marker::Absent => Basis::LastOnly,
        }
    }
}

impl From<alt_marker::Marker<State>> for Marker {
    fn from(read: alt_marker::Marker<State>) -> Marker {
        match read {
            alt_marker::Marker::Recorded(state) => Marker::Recorded(state),
            alt_marker::Marker::Legacy(head) => {
                let word = |at: usize| u64::from_le_bytes(head[at..at + 8].try_into().unwrap());
                Marker::Legacy([word(0), word(8), word(16)])
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

/// Records that the records up to `whole` are synced, for a writer that
/// read `generation` while it held the write lock, and with them the file
/// lengths `eof` of an fsync it made itself. Nothing is recorded when a
/// rewind has moved the generation since: it cut those records off.
/// Returns the boundaries the marker then holds, when it holds these.
pub(crate) fn record(
    path: &Path,
    generation: u64,
    whole: Lens,
    eof: Option<Lens>,
) -> std::io::Result<Option<Lens>> {
    let held = Cell::new(None);
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
        held.set(Some(state.whole));
        Some(state)
    })?;
    Ok(held.get())
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
/// hold: the caller holds the write lock and has read and fsynced every
/// file. A marker that records no generation gets one it never held.
pub(crate) fn establish(path: &Path, lens: Lens) -> std::io::Result<()> {
    update(path, |marker| {
        Some(State {
            generation: crate::odb::generation_kept(marker),
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

pub(crate) use crate::odb::sink::Deferred;

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

    #[test]
    fn an_older_marker_gives_the_lengths_it_recorded() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("odb.durable");
        assert_eq!(Marker::read(&path), Marker::Absent);
        let older: Vec<u8> = [7u64, 8, 9].iter().flat_map(|n| n.to_le_bytes()).collect();
        std::fs::write(&path, &older).unwrap();
        assert_eq!(Marker::read(&path), Marker::Legacy([7, 8, 9]));
        std::fs::write(&path, &older[..23]).unwrap();
        assert_eq!(Marker::read(&path), Marker::Unreadable);
        // an older marker is replaced whole
        std::fs::write(&path, &older).unwrap();
        set(&path, 10);
        assert_eq!(Marker::read(&path), Marker::Recorded(state(10)));
        let data = std::fs::read(&path).unwrap();
        assert_eq!((data.len(), &data[..24]), (224, &[0u8; 24][..]));
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
        let written = std::fs::read(&path).unwrap();
        // another pack than the one recorded, and a cut past the length
        lower_eof(&path, Log::Pack, 2, 40).unwrap();
        lower_eof(&path, Log::Tier1, 0, 100).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), written);
        lower_eof(&path, Log::Pack, 1, 40).unwrap();
        lower_eof(&path, Log::Map, 0, 60).unwrap();
        let mut expected = state(100);
        expected.eof.pack = 40;
        expected.eof.map = 60;
        assert_eq!(Marker::read(&path), Marker::Recorded(expected));
    }
}
