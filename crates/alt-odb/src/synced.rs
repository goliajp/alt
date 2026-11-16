//! The synced-length marker `odb.durable`: how far each append file has
//! been fsynced.
//!
//! It answers two questions. A writer about to fsync asks whether another
//! writer's fsync already covered its appends (group commit). An open that
//! finds a record it cannot read asks whether that record lies in the part
//! of the file that was synced — damage, reported — or past it — a write
//! that never finished, cut off (see [`alt_store::Tail`]).
//!
//! ```text
//! [active pack eof u64][blob map eof u64][map.alt eof u64]
//! [magic "ALTD"][pack seq u32]
//! [pack u64][blob map u64][tier1 u64][map.alt u64]
//! [checksum 8]
//! ```
//!
//! The first three numbers are file lengths at the last fsync, and all an
//! older alt reads or writes. The rest are record boundaries known synced:
//! a file length can include a dead writer's unfinished record, a boundary
//! cannot. The file is replaced by rename and written only after the fsync
//! it records (or before a cut it records), so at any moment it claims no
//! more than is on disk.

use std::path::Path;
use std::sync::{Arc, Mutex};

const MAGIC: [u8; 4] = *b"ALTD";
const LEN: usize = 72;
/// Every append file starts with a five-byte header.
pub(crate) const HEADER: u64 = 5;

/// Record boundaries in the four append files; `seq` names the active pack.
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

    fn reaches(&self, target: &Lens) -> bool {
        (self.seq, self.pack) >= (target.seq, target.pack)
            && self.blobmap >= target.blobmap
            && self.tier1 >= target.tier1
            && self.map >= target.map
    }

    /// How far pack `seq` is known synced. A pack later than the one
    /// recorded was created since, synced with its header and no further.
    pub fn pack_synced(&self, seq: u32) -> Option<u64> {
        match seq.cmp(&self.seq) {
            std::cmp::Ordering::Equal => Some(self.pack),
            std::cmp::Ordering::Greater => Some(HEADER),
            std::cmp::Ordering::Less => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Marker {
    /// file lengths at the last fsync: active pack, blob map, `map.alt`
    pub eofs: [u64; 3],
    /// record boundaries known synced; none in a marker an older alt wrote
    pub whole: Option<Lens>,
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
        let eofs = [word(0)?, word(8)?, word(16)?];
        let whole =
            (data.len() >= LEN && data[24..28] == MAGIC && check(&data[..64]) == data[64..72])
                .then(|| Lens {
                    seq: u32::from_le_bytes(data[28..32].try_into().unwrap()),
                    pack: word(32).unwrap(),
                    blobmap: word(40).unwrap(),
                    tier1: word(48).unwrap(),
                    map: word(56).unwrap(),
                });
        Some(Self { eofs, whole })
    }

    /// Replaces the marker (temp + rename), so a reader without the lock
    /// sees the old or the new one, never a torn one.
    pub fn write(path: &Path, eofs: [u64; 3], whole: Option<Lens>) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(LEN);
        for eof in eofs {
            buf.extend_from_slice(&eof.to_le_bytes());
        }
        if let Some(whole) = whole {
            buf.extend_from_slice(&MAGIC);
            buf.extend_from_slice(&whole.seq.to_le_bytes());
            for len in [whole.pack, whole.blobmap, whole.tier1, whole.map] {
                buf.extend_from_slice(&len.to_le_bytes());
            }
            let sum = check(&buf);
            buf.extend_from_slice(&sum);
        }
        let tmp = path.with_extension("durable.tmp");
        std::fs::write(&tmp, buf)?;
        std::fs::rename(&tmp, path)
    }

    /// Records `whole` as synced, the file lengths with it.
    pub fn write_whole(path: &Path, whole: Lens) -> std::io::Result<()> {
        Self::write(path, [whole.pack, whole.blobmap, whole.map], Some(whole))
    }

    /// Whether an fsync already covered appends up to `target`. A marker
    /// an older alt wrote names no pack and no tier1 length: its writer
    /// synced the tier1 file with every fsync, and its pack length is
    /// taken for the target's pack as that writer would take it.
    pub fn covers(&self, target: &Lens) -> bool {
        let pack = match self.whole {
            Some(whole) if whole.seq != target.seq => whole.seq > target.seq,
            _ => self.eofs[0] >= target.pack,
        };
        pack && self.eofs[1] >= target.blobmap
            && self.eofs[2] >= target.map
            && self.whole.is_none_or(|whole| whole.tier1 >= target.tier1)
    }

    /// Whether the record boundaries of `target` are recorded as synced.
    pub fn records(&self, target: &Lens) -> bool {
        self.whole.is_some_and(|whole| whole.reaches(target))
    }
}

/// What a handle whose fsyncs are left to a sink (the daemon) last finished
/// writing, for that sink to record once it has synced: the boundaries, and
/// a count that a rewind moves so a sink that read them before the cut does
/// not record them after it.
#[derive(Default)]
pub(crate) struct Pending {
    pub lens: Option<Lens>,
    pub rewinds: u64,
}

pub(crate) type Deferred = Arc<Mutex<Pending>>;
