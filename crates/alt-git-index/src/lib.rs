//! Git index (dircache) reading: versions 2–4. Entries are parsed fully;
//! extensions are preserved raw (signature + payload) so nothing is lost.
//! Business-agnostic stone.

use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};
use bstr::{BString, ByteSlice};
use sha1::digest::Digest;

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
            return Err(ERR("not an index file (bad magic)"));
        }
        let version = u32::from_be_bytes(data[4..8].try_into().unwrap());
        if !(2..=4).contains(&version) {
            return Err(ERR("unsupported index version"));
        }
        let count = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;

        // trailer: hash of everything before it
        let content_end = data.len() - raw;
        let checksum = &data[content_end..];
        let computed: Vec<u8> = match algo {
            HashAlgo::Sha1 => sha1::Sha1::digest(&data[..content_end]).to_vec(),
            HashAlgo::Sha256 => sha2::Sha256::digest(&data[..content_end]).to_vec(),
        };
        if checksum != computed {
            return Err(ERR("index checksum mismatch"));
        }

        let mut pos = 12;
        let mut entries = Vec::with_capacity(count);
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
        v = ((v + 1) << 7) | u64::from(b & 0x7f);
    }
    Ok(v)
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("index format: {0}")]
    Format(&'static str),
}
