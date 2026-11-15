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
//! [magic "ALTH"][version u8][reserved 3]
//! [pack count u16][run counts: chunks, blob map, tier1, map.alt u8 each][reserved 2]
//! packs x [seq u32]
//! runs  x [start u64][end u64]      for each of the four, in that order
//! [checksum 8]
//! ```
//!
//! A count of all ones says that part is not recorded.

use std::fs::File;
use std::os::unix::fs::FileExt;

use alt_store::Known;

use crate::odb::Odb;

const MAGIC: [u8; 4] = *b"ALTH";
const VERSION: u8 = 1;
const FIXED: usize = 16;
const MAX_PACKS: usize = 96;
const MAX_RUNS: usize = 12;
const MAX_LEN: usize = FIXED + 4 * MAX_PACKS + 4 * 16 * MAX_RUNS + 8;

type Chain = Option<Vec<(u64, u64)>>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Head {
    /// packs, chunk index runs and blob map runs
    pub store: Known,
    pub tier1: Chain,
    pub map: Chain,
}

fn check(body: &[u8]) -> [u8; 8] {
    alt_hash::blake3::hash(body).as_bytes()[..8]
        .try_into()
        .unwrap()
}

impl Head {
    /// What the lock file records; nothing when it records nothing usable.
    pub fn read(lock: &File) -> Self {
        let mut buf = [0u8; MAX_LEN];
        let have = lock.read_at(&mut buf, 0).unwrap_or(0);
        Self::parse(&buf[..have]).unwrap_or_default()
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
        out.extend_from_slice(&[VERSION, 0, 0, 0]);
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
        };
        let read = Head::parse(&head.encode()).unwrap();
        assert_eq!(read.store.packs, None);
        assert_eq!(read.store.chunk_runs, None);
        assert_eq!(read.store.map_runs, Some(vec![(5, 85)]));
    }
}
