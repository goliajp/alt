use std::fs::File;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};
use memmap2::Mmap;

use crate::PackError;

const IDX_MAGIC: [u8; 4] = [0xff, b't', b'O', b'c'];
/// Offset words with the MSB set index into the large-offset table.
const LARGE_OFFSET_FLAG: u32 = 1 << 31;

/// A memory-mapped pack index (`.idx`, version 2).
///
/// Layout: magic, version, 256-entry fan-out, N raw oids (sorted),
/// N crc32s, N 4-byte offsets (MSB → large table), large offsets,
/// pack checksum, idx checksum.
pub struct PackIndex {
    map: Mmap,
    algo: HashAlgo,
    len: u32,
    oids_at: usize,
    offsets_at: usize,
    large_at: usize,
    large_count: usize,
}

impl PackIndex {
    pub fn open(path: &Path, algo: HashAlgo) -> Result<Self, PackError> {
        let file = File::open(path)?;
        // Safety: the map is read-only and packs are append-only by
        // convention; a concurrently rewritten idx fails checksum-level
        // validation downstream rather than causing UB on these reads.
        let map = unsafe { Mmap::map(&file)? };
        Self::parse(map, algo)
    }

    fn parse(map: Mmap, algo: HashAlgo) -> Result<Self, PackError> {
        let data = &map[..];
        if data.len() < 8 + 256 * 4 || data[..4] != IDX_MAGIC {
            return Err(PackError::Format("not a v2 pack index (bad magic)"));
        }
        if read_u32(data, 4) != 2 {
            return Err(PackError::Format("unsupported pack index version"));
        }
        let fanout_at = 8;
        let len = read_u32(data, fanout_at + 255 * 4);
        // fan-out must be monotonic
        let mut prev = 0;
        for i in 0..256 {
            let v = read_u32(data, fanout_at + i * 4);
            if v < prev {
                return Err(PackError::Format("non-monotonic idx fan-out"));
            }
            prev = v;
        }

        let n = len as usize;
        let raw = algo.raw_len();
        let oids_at = fanout_at + 256 * 4;
        let crcs_at = oids_at + n * raw;
        let offsets_at = crcs_at + n * 4;
        let large_at = offsets_at + n * 4;
        let trailer = 2 * raw;
        if data.len() < large_at + trailer {
            return Err(PackError::Format("pack index truncated"));
        }
        let large_count = (data.len() - trailer - large_at) / 8;

        Ok(Self {
            map,
            algo,
            len,
            oids_at,
            offsets_at,
            large_at,
            large_count,
        })
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn algo(&self) -> HashAlgo {
        self.algo
    }

    /// The oid of entry `i` (entries are sorted by oid).
    pub fn oid_at(&self, i: u32) -> ObjectId {
        let raw = self.algo.raw_len();
        let at = self.oids_at + i as usize * raw;
        ObjectId::from_bytes(self.algo, &self.map[at..at + raw]).unwrap()
    }

    /// The pack-file byte offset of entry `i`.
    pub fn offset_at(&self, i: u32) -> Result<u64, PackError> {
        let word = read_u32(&self.map, self.offsets_at + i as usize * 4);
        if word & LARGE_OFFSET_FLAG == 0 {
            return Ok(u64::from(word));
        }
        let idx = (word & !LARGE_OFFSET_FLAG) as usize;
        if idx >= self.large_count {
            return Err(PackError::Format("large-offset index out of range"));
        }
        let at = self.large_at + idx * 8;
        let bytes: [u8; 8] = self.map[at..at + 8].try_into().unwrap();
        Ok(u64::from_be_bytes(bytes))
    }

    /// Binary-searches `oid` within its fan-out bucket.
    pub fn lookup(&self, oid: &ObjectId) -> Option<u32> {
        let want = oid.as_bytes();
        let bucket = want[0] as usize;
        let fanout_at = 8;
        let mut lo = if bucket == 0 {
            0
        } else {
            read_u32(&self.map, fanout_at + (bucket - 1) * 4)
        };
        let mut hi = read_u32(&self.map, fanout_at + bucket * 4);
        let raw = self.algo.raw_len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let at = self.oids_at + mid as usize * raw;
            match self.map[at..at + raw].cmp(want) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }
}

pub(crate) fn read_u32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
}
