use std::fs::File;
use std::io::Read;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use flate2::read::ZlibDecoder;
use memmap2::Mmap;

use crate::PackError;
use crate::idx::read_u32;

/// An entry header as stored in the pack: its kind, inflated size, and where
/// the zlib stream starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryInfo {
    pub kind: EntryKind,
    pub size: u64,
    pub data_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Plain(ObjectKind),
    /// Delta against the entry at the given absolute pack offset.
    OfsDelta {
        base_at: u64,
    },
    /// Delta against the object with the given id.
    RefDelta {
        base: ObjectId,
    },
}

/// A memory-mapped `.pack` file (version 2).
pub struct Pack {
    map: Mmap,
    algo: HashAlgo,
}

impl Pack {
    pub fn open(path: &Path, algo: HashAlgo) -> Result<Self, PackError> {
        let file = File::open(path)?;
        // Safety: see PackIndex::open.
        let map = unsafe { Mmap::map(&file)? };
        let data = &map[..];
        if data.len() < 12 + algo.raw_len() || &data[..4] != b"PACK" {
            return Err(PackError::Format("not a pack file (bad magic)"));
        }
        // git accepts versions 2 and 3 (3 shares the v2 layout)
        if !matches!(read_u32(data, 4), 2 | 3) {
            return Err(PackError::Format("unsupported pack version"));
        }
        Ok(Self { map, algo })
    }

    pub fn algo(&self) -> HashAlgo {
        self.algo
    }

    /// The object count from the pack header.
    pub fn object_count(&self) -> u32 {
        read_u32(&self.map, 8)
    }

    /// Parses the entry header at `offset`.
    pub fn entry_info(&self, offset: u64) -> Result<EntryInfo, PackError> {
        let data = &self.map[..];
        let mut pos = offset as usize;

        let mut byte = *data
            .get(pos)
            .ok_or(PackError::Format("entry offset out of range"))?;
        pos += 1;
        let type_id = (byte >> 4) & 0b111;
        let mut size = u64::from(byte & 0b1111);
        let mut shift = 4;
        while byte & 0x80 != 0 {
            byte = *data
                .get(pos)
                .ok_or(PackError::Format("truncated entry size"))?;
            pos += 1;
            size |= u64::from(byte & 0x7f) << shift;
            shift += 7;
        }

        let kind = match type_id {
            1 => EntryKind::Plain(ObjectKind::Commit),
            2 => EntryKind::Plain(ObjectKind::Tree),
            3 => EntryKind::Plain(ObjectKind::Blob),
            4 => EntryKind::Plain(ObjectKind::Tag),
            6 => {
                // distance encoding: each continuation adds (n+1) << 7
                byte = *data
                    .get(pos)
                    .ok_or(PackError::Format("truncated ofs-delta"))?;
                pos += 1;
                let mut dist = u64::from(byte & 0x7f);
                while byte & 0x80 != 0 {
                    byte = *data
                        .get(pos)
                        .ok_or(PackError::Format("truncated ofs-delta"))?;
                    pos += 1;
                    dist = ((dist + 1) << 7) | u64::from(byte & 0x7f);
                }
                let base_at = offset
                    .checked_sub(dist)
                    .ok_or(PackError::Format("ofs-delta points before pack start"))?;
                EntryKind::OfsDelta { base_at }
            }
            7 => {
                let raw = self.algo.raw_len();
                let bytes = data
                    .get(pos..pos + raw)
                    .ok_or(PackError::Format("truncated ref-delta base id"))?;
                pos += raw;
                EntryKind::RefDelta {
                    base: ObjectId::from_bytes(self.algo, bytes).unwrap(),
                }
            }
            _ => return Err(PackError::Format("invalid pack entry type")),
        };

        Ok(EntryInfo {
            kind,
            size,
            data_at: pos as u64,
        })
    }

    /// Inflates the zlib stream starting at `data_at` into exactly `size` bytes.
    pub fn inflate(&self, data_at: u64, size: u64) -> Result<Vec<u8>, PackError> {
        let mut out = Vec::with_capacity(size as usize);
        let mut decoder = ZlibDecoder::new(&self.map[data_at as usize..]);
        decoder.read_to_end(&mut out)?;
        if out.len() as u64 != size {
            return Err(PackError::Format(
                "inflated size does not match entry header",
            ));
        }
        Ok(out)
    }
}
