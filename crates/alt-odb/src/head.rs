//! The head record: which packs and index runs the store consists of,
//! written into the lock file so that opening the store costs one read of a
//! file it opens anyway instead of a listing of three directories.
//!
//! Writers replace it at the end of every batch, under the exclusive lock;
//! an opener reads it under the shared one, so never half-written. It is
//! advice (see [`Known`]): a program that does not write it — an older alt
//! — leaves it describing a store that has since changed, which every part
//! of it is checked for where it is used.
//!
//! ```text
//! [magic "ALTH"][version u8][flags u8][reserved 2]
//! [pack count u16][run counts: chunks, blob map, tier1, map.alt u8 each][reserved 2]
//! packs x [seq u32]
//! runs  x [start u64][end u64]      for each of the four, in that order
//! [checksum 8]
//! ```
//!
//! A count of all ones says that part is not recorded. Bit 0 of the flags
//! says the store has had a synced marker with record boundaries: when
//! the marker is then missing it was removed, not never written. An older
//! alt keeps no head record, and one that does writes the flags as zero.
//!
//! Past the longest head record the file holds one more thing an opener
//! wants without opening another file: how far the four append files are
//! known synced whole, as the synced marker had it when it was last raised.
//!
//! ```text
//! [pack seq u32][reserved 4]
//! [pack u64][blob map u64][tier1 u64][map.alt u64][checksum 8]      at byte 1176
//! ```
//!
//! It is written after the marker and never fsynced, so it can fall behind
//! the marker; an opener that finds a file reaching past it reads the
//! marker. It must never be ahead: whoever cuts a file back clears it and
//! fsyncs that first.
//!
//! After it comes the note of appends that were left unsynced
//! ([`crate::unsynced`]).

use std::fs::File;
use std::os::unix::fs::FileExt;

use alt_store::Known;

use crate::odb::Odb;
use crate::repair::{NOTED_AT, NOTES, Notes, notes};
use crate::synced::Lens;
use crate::unsynced::Left;

const MAGIC: [u8; 4] = *b"ALTH";
const VERSION: u8 = 1;
const FIXED: usize = 16;
const MAX_PACKS: usize = 96;
const MAX_RUNS: usize = 12;
const MAX_LEN: usize = FIXED + 4 * MAX_PACKS + 4 * 16 * MAX_RUNS + 8;
const SYNCED_AT: usize = MAX_LEN;
const SYNCED_LEN: usize = 48;
/// Where the note of unsynced appends is ([`crate::unsynced`]).
pub(crate) const UNSYNCED_AT: usize = SYNCED_AT + SYNCED_LEN;
const SYNCED_BODY: usize = SYNCED_LEN - 8;

type Chain = Option<Vec<(u64, u64)>>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Head {
    /// packs, chunk index runs and blob map runs
    pub store: Known,
    pub tier1: Chain,
    pub map: Chain,
    /// the store has had a synced marker with record boundaries
    pub bounded: bool,
}

pub(crate) fn check(body: &[u8]) -> [u8; 8] {
    alt_hash::blake3::hash(body).as_bytes()[..8]
        .try_into()
        .unwrap()
}

impl Head {
    /// What the lock file records; nothing when it records nothing usable.
    pub fn read(lock: &File) -> Self {
        Self::read_with_synced(lock).0
    }

    /// The same, how far the append files are known synced whole, when
    /// the file says, its note of unsynced appends, and what it notes of
    /// removals of lost records ([`crate::repair`]).
    pub fn read_with_synced(lock: &File) -> (Self, Option<Lens>, Option<Left>, Notes) {
        let mut buf = [0u8; NOTED_AT + NOTES];
        let have = lock.read_at(&mut buf, 0).unwrap_or(0);
        let head = Self::parse(&buf[..have.min(MAX_LEN)]).unwrap_or_default();
        let synced = buf[..have].get(SYNCED_AT..).and_then(parse_synced);
        let left = buf[..have].get(UNSYNCED_AT..).and_then(Left::parse);
        (head, synced, left, notes(buf[..have].get(NOTED_AT..)))
    }

    fn parse(data: &[u8]) -> Option<Self> {
        let fixed = data.get(..FIXED)?;
        if fixed[..4] != MAGIC || fixed[4] != VERSION {
            return None;
        }
        let packs = u16::from_le_bytes([fixed[8], fixed[9]]);
        let counts = [fixed[10], fixed[11], fixed[12], fixed[13]];
        let known = |count: u8| (count != u8::MAX).then_some(count as usize);
        let len = FIXED
            + 4 * (if packs == u16::MAX { 0 } else { packs as usize })
            + 16 * counts.iter().filter_map(|c| known(*c)).sum::<usize>();
        let body = data.get(..len)?;
        if check(body) != *data.get(len..len + 8)? {
            return None;
        }
        let mut at = FIXED;
        let mut word = |width: usize| {
            let mut bytes = [0u8; 8];
            bytes[..width].copy_from_slice(&body[at..at + width]);
            at += width;
            u64::from_le_bytes(bytes)
        };
        let packs = (packs != u16::MAX).then(|| (0..packs).map(|_| word(4) as u32).collect());
        let mut chain =
            |count: u8| -> Chain { Some((0..known(count)?).map(|_| (word(8), word(8))).collect()) };
        let (chunk_runs, map_runs) = (chain(counts[0]), chain(counts[1]));
        let (tier1, map) = (chain(counts[2]), chain(counts[3]));
        Some(Self {
            store: Known {
                packs,
                chunk_runs,
                map_runs,
            },
            tier1,
            map,
            bounded: fixed[5] & 1 != 0,
        })
    }

    fn encode(&self) -> Vec<u8> {
        let packs = self.store.packs.as_ref().filter(|p| p.len() <= MAX_PACKS);
        let chains = [
            &self.store.chunk_runs,
            &self.store.map_runs,
            &self.tier1,
            &self.map,
        ]
        .map(|chain| chain.as_ref().filter(|runs| runs.len() <= MAX_RUNS));
        let mut out = Vec::with_capacity(MAX_LEN);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&[VERSION, self.bounded as u8, 0, 0]);
        out.extend_from_slice(&packs.map_or(u16::MAX, |p| p.len() as u16).to_le_bytes());
        out.extend(
            chains
                .iter()
                .map(|c| c.map_or(u8::MAX, |runs| runs.len() as u8)),
        );
        out.extend_from_slice(&[0, 0]);
        for seq in packs.into_iter().flatten() {
            out.extend_from_slice(&seq.to_le_bytes());
        }
        for (start, end) in chains.into_iter().flatten().flatten() {
            out.extend_from_slice(&start.to_le_bytes());
            out.extend_from_slice(&end.to_le_bytes());
        }
        let sum = check(&out);
        out.extend_from_slice(&sum);
        out
    }
}

fn parse_synced(data: &[u8]) -> Option<Lens> {
    let (body, sum) = data.get(..SYNCED_LEN)?.split_at(SYNCED_BODY);
    if check(body) != *sum {
        return None;
    }
    let seq = u32::from_le_bytes(body[..4].try_into().unwrap());
    Some(Lens::decode(seq, &body[8..]))
}

/// Notes in the lock file that the files are synced whole up to `whole`,
/// for a caller that holds the lock (shared will do: no cut is under way)
/// and has seen the synced marker say so. Not fsynced, failures ignored:
/// without it an opener reads the marker.
pub(crate) fn note_synced(lock: &File, whole: Lens) {
    let mut bytes = Vec::with_capacity(SYNCED_LEN);
    bytes.extend_from_slice(&whole.seq.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    whole.encode(&mut bytes);
    let sum = check(&bytes);
    bytes.extend_from_slice(&sum);
    let _ = lock.write_all_at(&bytes, SYNCED_AT as u64);
}

/// Takes the note back before a file is cut, and fsyncs that: what grows
/// back there was not synced.
pub(crate) fn forget_synced(lock: &File) -> std::io::Result<()> {
    lock.write_all_at(&[0u8; SYNCED_LEN], SYNCED_AT as u64)?;
    lock.sync_data()
}

impl Odb {
    /// Records the store's files as this handle leaves them. Called with
    /// the write lock held, as the last thing a batch does. Not fsynced and
    /// failures ignored: a record that is lost, torn or stale only sends
    /// the next opener to the directories.
    pub(crate) fn save_head(&mut self) {
        let store = self.blobs.known();
        let head = Head {
            store: Known {
                // files this handle never opened are as it found them
                map_runs: store.map_runs.or_else(|| self.head.store.map_runs.take()),
                ..store
            },
            tier1: match self.tier1.get() {
                Some(tier1) => Some(tier1.run_chain()),
                None => self.head.tier1.take(),
            },
            map: Some(self.map.run_chain()),
            bounded: self.head.bounded,
        };
        if head != self.head_on_disk() {
            let _ = self.lock.write_all_at(&head.encode(), 0);
        }
        self.head = head;
    }

    /// What the lock file holds now: another writer may have replaced what
    /// this handle read when it opened.
    fn head_on_disk(&self) -> Head {
        Head::read(&self.lock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_reads_back_as_written_and_unrecorded_parts_stay_unrecorded() {
        let head = Head {
            store: Known {
                packs: Some(vec![3, 4, 9]),
                chunk_runs: Some(vec![(5, 100), (100, 2000)]),
                map_runs: None,
            },
            tier1: Some(Vec::new()),
            map: Some(vec![(5, 87)]),
            bounded: true,
        };
        let bytes = head.encode();
        assert_eq!(Head::parse(&bytes), Some(head.clone()));
        // trailing bytes of a longer, older record do not matter
        let mut longer = bytes.clone();
        longer.extend_from_slice(&[7; 40]);
        assert_eq!(Head::parse(&longer), Some(head));
        // a damaged or cut record is no record
        let mut damaged = bytes.clone();
        damaged[20] ^= 1;
        assert_eq!(Head::parse(&damaged), None);
        assert_eq!(Head::parse(&bytes[..bytes.len() - 1]), None);
        assert_eq!(Head::parse(b""), None);
    }

    #[test]
    fn parts_too_long_to_record_are_left_out() {
        let head = Head {
            store: Known {
                packs: Some((1..=200).collect()),
                chunk_runs: Some((0..40).map(|n| (n, n + 1)).collect()),
                map_runs: Some(vec![(5, 85)]),
            },
            tier1: None,
            map: None,
            bounded: false,
        };
        let read = Head::parse(&head.encode()).unwrap();
        assert_eq!(read.store.packs, None);
        assert_eq!(read.store.chunk_runs, None);
        assert_eq!(read.store.map_runs, Some(vec![(5, 85)]));
    }
}
