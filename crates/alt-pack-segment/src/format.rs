//! On-disk layout after the pack bytes, and the read-only view of a finished segment.
//!
//! ```text
//! pack bytes                       pack_len
//! fanout    256 x u32              records whose first oid byte is <= i
//! records   n x 88                 sorted by oid
//! commits   c x 52                 sorted by oid
//! parents   p x 20
//! footer    64
//! ```
//! All integers are little-endian. The footer checksum covers fanout through parents.

use crate::{Error, Kind, Oid};
use memmap2::Mmap;
use std::{fs::File, io::Write, path::Path};

pub(crate) const RECORD: usize = 88;
pub(crate) const COMMIT: usize = 52;
const FOOTER: usize = 64;
const MAGIC: &[u8; 8] = b"ALTPSEG1";
pub(crate) const NO_BASE: u32 = u32::MAX;

pub(crate) use alt_hash::crc32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delta {
    Plain,
    /// base is the record at `Record::base` of the same segment
    Inside,
    /// base lives in another segment; its id is in the pack entry
    Outside,
}

#[derive(Debug, Clone)]
pub struct Record {
    pub oid: Oid,
    pub blake3: [u8; 32],
    pub offset: u64,
    pub size: u64,
    pub crc: u32,
    pub len: u32,
    pub base: u32,
    pub kind: Kind,
    pub delta: Delta,
    pub depth: u16,
}

impl Record {
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.oid);
        out.extend_from_slice(&self.blake3);
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.size.to_le_bytes());
        out.extend_from_slice(&self.crc.to_le_bytes());
        out.extend_from_slice(&self.len.to_le_bytes());
        out.extend_from_slice(&self.base.to_le_bytes());
        out.push(self.kind as u8);
        out.push(match self.delta {
            Delta::Plain => 0,
            Delta::Inside => 1,
            Delta::Outside => 2,
        });
        out.extend_from_slice(&self.depth.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
    }

    fn decode(raw: &[u8]) -> Result<Self, Error> {
        let u32_at = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(raw[at..at + 8].try_into().unwrap());
        Ok(Self {
            oid: raw[..20].try_into().unwrap(),
            blake3: raw[20..52].try_into().unwrap(),
            offset: u64_at(52),
            size: u64_at(60),
            crc: u32_at(68),
            len: u32_at(72),
            base: u32_at(76),
            kind: Kind::from_code(raw[80]).ok_or(Error::Corrupt("record kind"))?,
            delta: match raw[81] {
                0 => Delta::Plain,
                1 => Delta::Inside,
                2 => Delta::Outside,
                _ => return Err(Error::Corrupt("record delta")),
            },
            depth: u16::from_le_bytes(raw[82..84].try_into().unwrap()),
        })
    }
}

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub tree: Oid,
    pub generation: u32,
    pub parents: Vec<Oid>,
}

pub(crate) struct CommitRow {
    pub oid: Oid,
    pub tree: Oid,
    pub generation: u32,
    pub parents: Vec<Oid>,
}

/// appends the index for a verified pack of `pack_len` bytes
pub(crate) fn write_index(
    records: &[Record],
    commits: &[CommitRow],
    flags: u32,
    pack_len: u64,
    out: &mut impl Write,
) -> Result<(), Error> {
    let mut body =
        Vec::with_capacity(1024 + records.len() * RECORD + commits.len() * (COMMIT + 20));
    let mut fanout = [0u32; 256];
    for record in records {
        fanout[record.oid[0] as usize] += 1;
    }
    let mut total = 0;
    for slot in &mut fanout {
        total += *slot;
        body.extend_from_slice(&total.to_le_bytes());
    }
    for record in records {
        record.encode(&mut body);
    }
    let mut parents = Vec::new();
    for commit in commits {
        body.extend_from_slice(&commit.oid);
        body.extend_from_slice(&commit.tree);
        body.extend_from_slice(&commit.generation.to_le_bytes());
        body.extend_from_slice(&((parents.len() / 20) as u32).to_le_bytes());
        body.extend_from_slice(&(commit.parents.len() as u32).to_le_bytes());
        for parent in &commit.parents {
            parents.extend_from_slice(parent);
        }
    }
    let parent_count = (parents.len() / 20) as u32;
    body.extend_from_slice(&parents);
    let mut footer = Vec::with_capacity(FOOTER);
    footer.extend_from_slice(&pack_len.to_le_bytes());
    footer.extend_from_slice(&(records.len() as u32).to_le_bytes());
    footer.extend_from_slice(&(commits.len() as u32).to_le_bytes());
    footer.extend_from_slice(&parent_count.to_le_bytes());
    footer.extend_from_slice(&flags.to_le_bytes());
    footer.extend_from_slice(blake3::hash(&body).as_bytes());
    footer.extend_from_slice(MAGIC);
    out.write_all(&body)?;
    out.write_all(&footer)?;
    Ok(())
}

pub struct Segment {
    map: Mmap,
    pack_len: usize,
    records: usize,
    commits: usize,
    fanout_at: usize,
    records_at: usize,
    commits_at: usize,
    parents_at: usize,
    parent_count: usize,
    flags: u32,
}

impl Segment {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let file = File::open(path)?;
        // the file is immutable once published; writers create it under a fresh name
        let map = unsafe { Mmap::map(&file)? };
        let bad = || Error::Corrupt("segment footer");
        let footer_at = map.len().checked_sub(FOOTER).ok_or_else(bad)?;
        let footer = &map[footer_at..];
        if &footer[56..] != MAGIC {
            return Err(bad());
        }
        let u32_at =
            |at: usize| u32::from_le_bytes(footer[at..at + 4].try_into().unwrap()) as usize;
        let pack_len = u64::from_le_bytes(footer[..8].try_into().unwrap()) as usize;
        let (records, commits, parent_count) = (u32_at(8), u32_at(12), u32_at(16));
        let fanout_at = pack_len;
        let records_at = fanout_at + 1024;
        let commits_at = records_at + records * RECORD;
        let parents_at = commits_at + commits * COMMIT;
        if pack_len < 32 || parents_at + parent_count * 20 != footer_at {
            return Err(bad());
        }
        if blake3::hash(&map[fanout_at..footer_at]).as_bytes() != &footer[24..56] {
            return Err(Error::Corrupt("segment index checksum"));
        }
        Ok(Self {
            flags: u32_at(20) as u32,
            map,
            pack_len,
            records,
            commits,
            fanout_at,
            records_at,
            commits_at,
            parents_at,
            parent_count,
        })
    }

    pub fn pack(&self) -> &[u8] {
        &self.map[..self.pack_len]
    }

    pub fn len(&self) -> usize {
        self.records
    }

    pub fn is_empty(&self) -> bool {
        self.records == 0
    }

    pub fn flags(&self) -> u32 {
        self.flags
    }

    fn oid_at(&self, index: usize) -> &[u8] {
        let at = self.records_at + index * RECORD;
        &self.map[at..at + 20]
    }

    pub fn record(&self, index: usize) -> Result<Record, Error> {
        if index >= self.records {
            return Err(Error::Corrupt("record index"));
        }
        let at = self.records_at + index * RECORD;
        Record::decode(&self.map[at..at + RECORD])
    }

    pub fn find(&self, oid: &Oid) -> Option<usize> {
        let slot = |byte: usize| {
            let at = self.fanout_at + byte * 4;
            u32::from_le_bytes(self.map[at..at + 4].try_into().unwrap()) as usize
        };
        let first = oid[0] as usize;
        let (mut low, mut high) = (if first == 0 { 0 } else { slot(first - 1) }, slot(first));
        while low < high {
            let middle = (low + high) / 2;
            match self.oid_at(middle).cmp(oid.as_slice()) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Some(middle),
            }
        }
        None
    }

    pub fn commit(&self, oid: &Oid) -> Option<CommitInfo> {
        let row = |index: usize| {
            let at = self.commits_at + index * COMMIT;
            &self.map[at..at + COMMIT]
        };
        let (mut low, mut high) = (0, self.commits);
        while low < high {
            let middle = (low + high) / 2;
            let raw = row(middle);
            match raw[..20].cmp(oid.as_slice()) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => {
                    let u32_at =
                        |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
                    let (start, count) = (u32_at(44) as usize, u32_at(48) as usize);
                    if start + count > self.parent_count {
                        return None;
                    }
                    let parents = (start..start + count)
                        .map(|index| {
                            let at = self.parents_at + index * 20;
                            self.map[at..at + 20].try_into().unwrap()
                        })
                        .collect();
                    return Some(CommitInfo {
                        tree: raw[20..40].try_into().unwrap(),
                        generation: u32_at(40),
                        parents,
                    });
                }
            }
        }
        None
    }

    /// the stored bytes of one pack entry, checked against its checksum
    pub(crate) fn entry(&self, record: &Record) -> Result<&[u8], Error> {
        let start = record.offset as usize;
        let bytes = start
            .checked_add(record.len as usize)
            .filter(|end| *end <= self.pack_len - 20)
            .map(|end| &self.map[start..end])
            .ok_or(Error::Corrupt("entry range"))?;
        if crc32(bytes) != record.crc {
            return Err(Error::Corrupt("entry checksum"));
        }
        Ok(bytes)
    }
}
