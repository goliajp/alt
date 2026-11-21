//! The snapshot's file: the snapshot as it was when the file was last
//! written whole, and after that one record for each time something was
//! learned or the index replaced since. A command that learns a little
//! adds a record instead of writing the whole again.
//!
//! The file is never synced. What did not reach the disk whole fails a
//! checksum: the whole part its own, a record the one that chains it to
//! everything before it. The records up to the first that fails are what
//! the file says.
//!
//! The whole part of a file is never written again: records come after
//! it, and a snapshot written whole is another file under the same name.

use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};

use super::{Dir, Entry, FileStat, Snapshot};

const MAGIC: &[u8; 8] = b"ALTWTC01";
pub(super) const HEADER: usize = 80;
const ENTRY: usize = 104;
const DIR: usize = 56;
pub(super) const CHECKSUM: usize = 32;
/// the bytes a stat takes in an entry and in a record
pub(super) const STAT: usize = 52;

/// Records are added while they take less than this share of the whole
/// part, and no less than this many bytes are allowed them.
const RECORDS_SHARE: u64 = 4;
const RECORDS_AT_LEAST: u64 = 16 << 10;

/// The file a snapshot was read from or written to, as far as that went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Origin {
    pub(super) dev: u64,
    pub(super) ino: u64,
    /// the length of the whole part
    pub(super) whole: u64,
    /// the length up to the end of the last record read or written
    pub(super) len: u64,
    /// the checksum the next record is chained to
    pub(super) chain: [u8; CHECKSUM],
}

/// Where the parts of a whole part lie, as its header says.
pub(super) struct Laid {
    id_len: usize,
    entries: usize,
    paths_at: usize,
    dirs_at: usize,
    dirs: usize,
    /// where its checksum stands
    body: usize,
}

impl Laid {
    /// What the header at the start of `bytes` says, where it is one of a
    /// snapshot of this version; the header is the first [`HEADER`] bytes.
    pub(super) fn of(bytes: &[u8], algo: HashAlgo) -> Option<Laid> {
        if bytes.len() < HEADER || &bytes[..8] != MAGIC || u32le(bytes, 8) != 1 {
            return None;
        }
        if u32le(bytes, 12) as usize != algo.raw_len() {
            return None;
        }
        let id_len = u64le(bytes, 48) as usize;
        let (entries, paths_len, dirs) = (
            u64le(bytes, 56) as usize,
            u64le(bytes, 64) as usize,
            u64le(bytes, 72) as usize,
        );
        let paths_at = HEADER.checked_add(entries.checked_mul(ENTRY)?)?;
        let dirs_at = paths_at.checked_add(paths_len)?;
        let body = dirs_at.checked_add(dirs.checked_mul(DIR)?)?;
        body.checked_add(CHECKSUM)?;
        (id_len <= 32).then_some(Laid {
            id_len,
            entries,
            paths_at,
            dirs_at,
            dirs,
            body,
        })
    }

    /// The length of the whole part.
    pub(super) fn whole(&self) -> usize {
        self.body + CHECKSUM
    }

    /// Whether the whole part at the start of `bytes` is as it was written.
    pub(super) fn holds(&self, bytes: &[u8]) -> bool {
        alt_hash::blake3::hash(&bytes[..self.body]).as_bytes()[..] == bytes[self.body..self.whole()]
    }
}

impl Origin {
    /// Whether records of `more` bytes on top of those there still are few.
    pub(super) fn takes(&self, more: usize) -> bool {
        let allowed = (self.whole / RECORDS_SHARE).max(RECORDS_AT_LEAST);
        self.len - self.whole + more as u64 <= allowed
    }
}

impl Snapshot {
    /// The whole part of the snapshot's file: a file that has no records yet.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            HEADER
                + self.entries.len() * ENTRY
                + self.paths.len()
                + self.dirs.len() * DIR
                + CHECKSUM,
        );
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(self.algo.raw_len() as u32).to_le_bytes());
        let mut id = [0u8; 32];
        id[..self.index_id.len()].copy_from_slice(&self.index_id);
        out.extend_from_slice(&id);
        out.extend_from_slice(&(self.index_id.len() as u64).to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u64).to_le_bytes());
        out.extend_from_slice(&(self.paths.len() as u64).to_le_bytes());
        out.extend_from_slice(&(self.dirs.len() as u64).to_le_bytes());
        for e in &self.entries {
            out.extend_from_slice(&e.path.0.to_le_bytes());
            out.extend_from_slice(&e.path.1.to_le_bytes());
            out.extend_from_slice(&e.mode.to_le_bytes());
            out.extend_from_slice(&e.flags.to_le_bytes());
            out.extend_from_slice(&[u8::from(e.stat.is_some()), 0]);
            put_oid(&mut out, Some(&e.oid));
            put_stat(&mut out, e.stat.as_ref());
            out.extend_from_slice(&[0; 4]);
        }
        out.extend_from_slice(&self.paths);
        for d in &self.dirs {
            out.extend_from_slice(&d.first.to_le_bytes());
            out.extend_from_slice(&d.end.to_le_bytes());
            out.extend_from_slice(&d.path_len.to_le_bytes());
            out.extend_from_slice(&d.descendants.to_le_bytes());
            out.extend_from_slice(&[u8::from(d.tree.is_some()), 0, 0, 0, 0, 0, 0, 0]);
            put_oid(&mut out, d.tree.as_ref());
        }
        let sum = alt_hash::blake3::hash(&out);
        out.extend_from_slice(sum.as_bytes());
        out
    }

    /// The snapshot the whole part at the start of `bytes` says, laid
    /// out as `laid` has it.
    pub(super) fn decode_whole(bytes: &[u8], algo: HashAlgo, laid: &Laid) -> Option<Snapshot> {
        let (n, n_dirs, id_len) = (laid.entries, laid.dirs, laid.id_len);
        let (paths_at, dirs_at) = (laid.paths_at, laid.dirs_at);
        let paths_len = dirs_at - paths_at;
        let raw = algo.raw_len();
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let b = &bytes[HEADER + i * ENTRY..][..ENTRY];
            let path = (u32le(b, 0), u32le(b, 4));
            if path.0 as usize + path.1 as usize > paths_len {
                return None;
            }
            entries.push(Entry {
                path,
                mode: u32le(b, 8),
                flags: u16::from_le_bytes([b[12], b[13]]),
                oid: ObjectId::from_bytes(algo, &b[16..16 + raw]).ok()?,
                stat: (b[14] != 0).then(|| stat_at(&b[48..])),
            });
        }
        let mut dirs = Vec::with_capacity(n_dirs);
        for i in 0..n_dirs {
            let b = &bytes[dirs_at + i * DIR..][..DIR];
            let d = Dir {
                first: u32le(b, 0),
                end: u32le(b, 4),
                path_len: u32le(b, 8),
                descendants: u32le(b, 12),
                tree: match b[16] {
                    0 => None,
                    _ => Some(ObjectId::from_bytes(algo, &b[24..24 + raw]).ok()?),
                },
            };
            if d.first > d.end || d.end as usize > n || i + d.descendants as usize >= n_dirs {
                return None;
            }
            dirs.push(d);
        }
        if dirs.is_empty() {
            return None;
        }
        Some(Snapshot {
            index_id: bytes[16..16 + id_len].to_vec(),
            algo,
            paths: bytes[paths_at..dirs_at].to_vec(),
            entries,
            dirs,
            changed: false,
            origin: None,
            noted: Vec::new(),
        })
    }

    /// Has the file at `path` say what this says: with one more record
    /// where the file is still as this left it and the record is small
    /// beside it, else written whole in one step. Nothing is synced.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        if !self.add_record(path)? {
            let bytes = self.encode();
            let meta = crate::control::put(path, &bytes)?;
            self.origin = Some(Origin {
                dev: meta.dev(),
                ino: meta.ino(),
                whole: bytes.len() as u64,
                len: bytes.len() as u64,
                chain: bytes[bytes.len() - CHECKSUM..].try_into().unwrap(),
            });
        }
        self.noted.clear();
        self.changed = false;
        Ok(())
    }

    /// Adds what was noted as a record of the file at `path`, unless that
    /// file is not, or no more, the one this stands for up to its end.
    /// Two processes that add a record at the same moment leave one of
    /// the two, or one and a rest that fails its checksum.
    fn add_record(&mut self, path: &Path) -> std::io::Result<bool> {
        let Some(origin) = &mut self.origin else {
            return Ok(false);
        };
        if self.noted.is_empty() {
            return Ok(true);
        }
        let file = match std::fs::OpenOptions::new().write(true).open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        let meta = file.metadata()?;
        if (meta.dev(), meta.ino(), meta.len()) != (origin.dev, origin.ino, origin.len) {
            return Ok(false);
        }
        let (record, sum) = super::journal::sealed(&self.noted, &origin.chain);
        file.write_all_at(&record, origin.len)?;
        origin.len += record.len() as u64;
        origin.chain = sum;
        Ok(true)
    }
}

pub(super) fn put_oid(out: &mut Vec<u8>, oid: Option<&ObjectId>) {
    let mut raw = [0u8; 32];
    if let Some(oid) = oid {
        raw[..oid.as_bytes().len()].copy_from_slice(oid.as_bytes());
    }
    out.extend_from_slice(&raw);
}

/// A stat in [`STAT`] bytes, zeros for none.
pub(super) fn put_stat(out: &mut Vec<u8>, stat: Option<&FileStat>) {
    let Some(s) = stat else {
        return out.extend_from_slice(&[0; STAT]);
    };
    out.extend_from_slice(&s.mtime.0.to_le_bytes());
    out.extend_from_slice(&s.ctime.0.to_le_bytes());
    out.extend_from_slice(&s.mtime.1.to_le_bytes());
    out.extend_from_slice(&s.ctime.1.to_le_bytes());
    out.extend_from_slice(&s.dev.to_le_bytes());
    out.extend_from_slice(&s.ino.to_le_bytes());
    out.extend_from_slice(&s.size.to_le_bytes());
    out.extend_from_slice(&s.mode.to_le_bytes());
}

/// The stat [`put_stat`] left at the start of `b`.
pub(super) fn stat_at(b: &[u8]) -> FileStat {
    FileStat {
        mtime: (u64le(b, 0) as i64, u32le(b, 16)),
        ctime: (u64le(b, 8) as i64, u32le(b, 20)),
        dev: u64le(b, 24),
        ino: u64le(b, 32),
        size: u64le(b, 40),
        mode: u32le(b, 48),
    }
}

pub(super) fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub(super) fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{index, oid, stat};
    use super::*;

    #[test]
    fn encoding_round_trips_and_damage_is_refused() {
        let i = index(&[("a", 1), ("b/c", 2)]);
        let mut s = Snapshot::of_index(&i, &[7; 20], HashAlgo::Sha1, None);
        s.set_stat(1, Some(stat(9)));
        s.dirs[1].tree = Some(oid(8));
        let bytes = s.encode();
        let back = Snapshot::decode(&bytes, HashAlgo::Sha1).unwrap();
        assert!(!back.changed);
        assert_eq!(back.entries, s.entries);
        assert_eq!(back.dirs, s.dirs);
        assert_eq!(back.index_id, vec![7; 20]);
        assert_eq!(back.path(1), b"b/c");
        for at in [0, 20, HEADER + 3, bytes.len() - 1] {
            let mut bad = bytes.clone();
            bad[at] ^= 1;
            assert!(
                Snapshot::decode(&bad, HashAlgo::Sha1).is_none(),
                "byte {at}"
            );
        }
        assert!(Snapshot::decode(&bytes[..bytes.len() - 1], HashAlgo::Sha1).is_none());
        assert!(Snapshot::decode(&bytes, HashAlgo::Sha256).is_none());
    }
}
