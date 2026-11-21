//! Git index (dircache) reading: versions 2–4. Entries are parsed fully;
//! extensions are preserved raw (signature + payload) so nothing is lost.
//! Pure-logic crate, business-agnostic.

use std::path::Path;

use alt_bytes::{BString, ByteSlice};
use alt_git_codec::{HashAlgo, ObjectId};

mod clash;
mod serialize;
pub use clash::clash_among;

/// Stage bits and friends live in `flags`; typed accessors below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub ctime: (u32, u32),
    pub mtime: (u32, u32),
    pub dev: u32,
    pub ino: u32,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u32,
    pub oid: ObjectId,
    pub flags: u16,
    /// Present when the extended bit is set (v3+): skip-worktree,
    /// intent-to-add.
    pub extended_flags: Option<u16>,
    pub path: BString,
}

impl IndexEntry {
    pub fn stage(&self) -> u8 {
        ((self.flags >> 12) & 0b11) as u8
    }

    pub fn assume_valid(&self) -> bool {
        self.flags & 0x8000 != 0
    }

    pub fn skip_worktree(&self) -> bool {
        self.extended_flags.is_some_and(|f| f & 0x4000 != 0)
    }

    pub fn intent_to_add(&self) -> bool {
        self.extended_flags.is_some_and(|f| f & 0x2000 != 0)
    }
}

/// An extension chunk, kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub signature: [u8; 4],
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Index {
    pub version: u32,
    pub entries: Vec<IndexEntry>,
    pub extensions: Vec<Extension>,
}

impl Index {
    pub fn open(path: &Path, algo: HashAlgo) -> Result<Self, IndexError> {
        Self::parse(&std::fs::read(path)?, algo)
    }

    pub fn parse(data: &[u8], algo: HashAlgo) -> Result<Self, IndexError> {
        const ERR: fn(&'static str) -> IndexError = IndexError::Format;
        let raw = algo.raw_len();
        if data.len() < 12 + raw || &data[..4] != b"DIRC" {
            return Err(IndexError::Torn("not an index file (bad magic)"));
        }

        // trailer: hash of everything before it
        let content_end = data.len() - raw;
        let checksum = &data[content_end..];
        let computed: Vec<u8> = match algo {
            HashAlgo::Sha1 => alt_hash::sha1(&data[..content_end]).to_vec(),
            HashAlgo::Sha256 => alt_hash::sha256(&data[..content_end]).to_vec(),
        };
        if checksum != computed {
            return Err(IndexError::Torn("index checksum mismatch"));
        }
        let version = u32::from_be_bytes(data[4..8].try_into().unwrap());
        if !(2..=4).contains(&version) {
            return Err(ERR("unsupported index version"));
        }
        let count = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;

        let mut pos = 12;
        // `count` is an untrusted header field; never pre-allocate it
        // unbounded (fuzz found a 4-billion-entry reservation OOM). An entry
        // is at least 8 bytes, so the file size caps how many can exist.
        let mut entries = Vec::with_capacity(count.min(data.len() / 8));
        let mut prev_path: Vec<u8> = Vec::new();
        for _ in 0..count {
            let entry = parse_entry(data, &mut pos, version, algo, &mut prev_path)?;
            entries.push(entry);
        }

        let mut extensions = Vec::new();
        while pos < content_end {
            if pos + 8 > content_end {
                return Err(ERR("truncated extension header"));
            }
            let signature: [u8; 4] = data[pos..pos + 4].try_into().unwrap();
            let size = u32::from_be_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
            pos += 8;
            let payload = data
                .get(pos..pos + size)
                .ok_or(ERR("extension payload overruns file"))?;
            pos += size;
            extensions.push(Extension {
                signature,
                data: payload.to_vec(),
            });
        }

        Ok(Self {
            version,
            entries,
            extensions,
        })
    }
}

fn parse_entry(
    data: &[u8],
    pos: &mut usize,
    version: u32,
    algo: HashAlgo,
    prev_path: &mut Vec<u8>,
) -> Result<IndexEntry, IndexError> {
    const ERR: fn(&'static str) -> IndexError = IndexError::Format;
    let raw = algo.raw_len();
    let start = *pos;
    let fixed = data
        .get(start..start + 40 + raw + 2)
        .ok_or(ERR("truncated index entry"))?;
    let be = |i: usize| u32::from_be_bytes(fixed[i..i + 4].try_into().unwrap());

    let flags = u16::from_be_bytes(fixed[40 + raw..40 + raw + 2].try_into().unwrap());
    *pos = start + 40 + raw + 2;

    let extended_flags = if flags & 0x4000 != 0 {
        if version < 3 {
            return Err(ERR("extended flag in a v2 index"));
        }
        let ext = data
            .get(*pos..*pos + 2)
            .ok_or(ERR("truncated extended flags"))?;
        *pos += 2;
        Some(u16::from_be_bytes(ext.try_into().unwrap()))
    } else {
        None
    };

    let name_len = (flags & 0x0FFF) as usize;
    let path: BString = if version == 4 {
        // prefix compression: strip N bytes from the previous path, then
        // append the NUL-terminated suffix
        let strip = chained_varint(data, pos)? as usize;
        let rest = &data[*pos..];
        let nul = rest.find_byte(0).ok_or(ERR("unterminated v4 path"))?;
        let suffix = &rest[..nul];
        *pos += nul + 1;
        if strip > prev_path.len() {
            return Err(ERR("v4 prefix strip exceeds previous path"));
        }
        prev_path.truncate(prev_path.len() - strip);
        prev_path.extend_from_slice(suffix);
        prev_path.as_slice().into()
    } else {
        let name = if name_len == 0x0FFF {
            let rest = &data[*pos..];
            let nul = rest.find_byte(0).ok_or(ERR("unterminated long path"))?;
            &rest[..nul]
        } else {
            data.get(*pos..*pos + name_len)
                .ok_or(ERR("truncated entry path"))?
        };
        *pos += name.len();
        // pad with 1-8 NULs to an 8-byte multiple of the whole entry
        let entry_len = *pos - start;
        let pad = 8 - (entry_len % 8);
        if data
            .get(*pos..*pos + pad)
            .is_none_or(|p| p.iter().any(|&b| b != 0))
        {
            return Err(ERR("bad entry padding"));
        }
        *pos += pad;
        name.into()
    };

    Ok(IndexEntry {
        ctime: (be(0), be(4)),
        mtime: (be(8), be(12)),
        dev: be(16),
        ino: be(20),
        mode: be(24),
        uid: be(28),
        gid: be(32),
        size: be(36),
        oid: ObjectId::from_bytes(algo, &fixed[40..40 + raw]).unwrap(),
        flags,
        extended_flags,
        path,
    })
}

/// The chained varint shared by ofs-delta distances, index v4, and reftable.
fn chained_varint(d: &[u8], pos: &mut usize) -> Result<u64, IndexError> {
    let mut b = *d.get(*pos).ok_or(IndexError::Format("truncated varint"))?;
    *pos += 1;
    let mut v = u64::from(b & 0x7f);
    while b & 0x80 != 0 {
        b = *d.get(*pos).ok_or(IndexError::Format("truncated varint"))?;
        *pos += 1;
        // an over-long varint overflows the accumulator; reject rather than
        // panic (fuzz). Shared with ofs-delta / reftable, so this hardens
        // them too.
        let bumped = v
            .checked_add(1)
            .ok_or(IndexError::Format("varint overflow"))?;
        v = (bumped << 7) | u64::from(b & 0x7f);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_trailer(mut buf: Vec<u8>) -> Vec<u8> {
        let t = alt_hash::sha1(&buf);
        buf.extend_from_slice(&t);
        buf
    }

    #[test]
    fn huge_entry_count_is_rejected_without_allocating() {
        // DIRC v2 claiming u32::MAX entries with no body: fuzz found this
        // reserving four billion entries. It must Err on truncation instead.
        let mut buf = b"DIRC".to_vec();
        buf.extend_from_slice(&2u32.to_be_bytes());
        buf.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(Index::parse(&with_trailer(buf), HashAlgo::Sha1).is_err());
    }

    #[test]
    fn bytes_that_are_not_a_file_written_whole_are_told_from_an_index_that_does_not_read() {
        let mut header = b"DIRC".to_vec();
        header.extend_from_slice(&2u32.to_be_bytes());
        header.extend_from_slice(&0u32.to_be_bytes());
        let whole = with_trailer(header.clone());
        assert!(Index::parse(&whole, HashAlgo::Sha1).is_ok());
        let torn = |bytes: &[u8]| {
            matches!(
                Index::parse(bytes, HashAlgo::Sha1),
                Err(IndexError::Torn(_))
            )
        };
        // empty, cut anywhere, zeroed, and with one bit of the checksum off
        for cut in 0..whole.len() {
            assert!(torn(&whole[..cut]), "cut at {cut}");
        }
        assert!(torn(&vec![0; whole.len()]));
        let mut flipped = whole.clone();
        *flipped.last_mut().unwrap() ^= 1;
        assert!(torn(&flipped));
        // a version this does not read, under a checksum that holds, is no tear
        header[7] = 9;
        assert!(matches!(
            Index::parse(&with_trailer(header), HashAlgo::Sha1),
            Err(IndexError::Format(_))
        ));
    }
}

#[derive(Debug)]
pub enum IndexError {
    Io(std::io::Error),
    /// what its own checksum covers does not read as an index
    Format(&'static str),
    /// the bytes are not a file that was written whole: they are too
    /// short, do not begin as an index does, or do not end in the
    /// checksum of what comes before
    Torn(&'static str),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => f.write_str("io"),
            Self::Format(v) | Self::Torn(v) => write!(f, "index format: {v}"),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for IndexError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
