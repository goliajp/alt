//! The snapshot's file: the snapshot as it was when the file was last
//! written whole, and after that one record for each time something was
//! learned or the index replaced since. A command that learns a little
//! adds a record instead of writing the whole again.
//!
//! The file is never synced. What did not reach the disk whole fails a
//! checksum: the whole part its own, a record the one that chains it to
//! everything before it. The records up to the first that fails are what
//! the file says.

use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};

use super::{Dir, Entry, FileStat, Snapshot};

const MAGIC: &[u8; 8] = b"ALTWTC01";
const HEADER: usize = 80;
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
    dev: u64,
    ino: u64,
    /// the length of the whole part
    whole: u64,
    /// the length up to the end of the last record read or written
    len: u64,
    /// the checksum the next record is chained to
    chain: [u8; CHECKSUM],
}

impl Origin {
    /// Whether records of `more` bytes on top of those there still are few.
    pub(super) fn takes(&self, more: usize) -> bool {
        let allowed = (self.whole / RECORDS_SHARE).max(RECORDS_AT_LEAST);
        self.len - self.whole + more as u64 <= allowed
    }
}

impl Snapshot {
    /// The snapshot as a file that has no records yet.
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

    /// The snapshot `bytes` say; `None` when they do not begin with one
    /// this version wrote whole.
    pub fn decode(bytes: &[u8], algo: HashAlgo) -> Option<Snapshot> {
        Some(Snapshot::read(bytes, algo)?.0)
    }

    /// [`decode`](Self::decode), with the length of the whole part, the
    /// length up to the end of the last record that holds, and the
    /// checksum a record after it is chained to.
    fn read(bytes: &[u8], algo: HashAlgo) -> Option<(Snapshot, usize, usize, [u8; CHECKSUM])> {
        let (mut snapshot, whole) = Snapshot::decode_whole(bytes, algo)?;
        let mut chain: [u8; CHECKSUM] = bytes[whole - CHECKSUM..whole].try_into().unwrap();
        let mut len = whole;
        while let Some((noted, sum)) = super::journal::record(&bytes[len..], &chain) {
            // a record that holds and names what is not there is no
            // record of this snapshot: nothing of the file is believed
            snapshot.replay(noted)?;
            len += super::journal::framed(noted.len());
            chain = sum;
        }
        snapshot.changed = false;
        Some((snapshot, whole, len, chain))
    }

    fn decode_whole(bytes: &[u8], algo: HashAlgo) -> Option<(Snapshot, usize)> {
        if bytes.len() < HEADER || &bytes[..8] != MAGIC || u32le(bytes, 8) != 1 {
            return None;
        }
        if u32le(bytes, 12) as usize != algo.raw_len() {
            return None;
        }
        let id_len = u64le(bytes, 48) as usize;
        let (n, paths_len, n_dirs) = (
            u64le(bytes, 56) as usize,
            u64le(bytes, 64) as usize,
            u64le(bytes, 72) as usize,
        );
        let paths_at = HEADER.checked_add(n.checked_mul(ENTRY)?)?;
        let dirs_at = paths_at.checked_add(paths_len)?;
        let body = dirs_at.checked_add(n_dirs.checked_mul(DIR)?)?;
        let whole = body.checked_add(CHECKSUM)?;
        if id_len > 32 || bytes.len() < whole {
            return None;
        }
        if alt_hash::blake3::hash(&bytes[..body]).as_bytes()[..] != bytes[body..whole] {
            return None;
        }
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
        let snapshot = Snapshot {
            index_id: bytes[16..16 + id_len].to_vec(),
            algo,
            paths: bytes[paths_at..dirs_at].to_vec(),
            entries,
            dirs,
            changed: false,
            origin: None,
            noted: Vec::new(),
        };
        Some((snapshot, whole))
    }

    /// Reads the snapshot at `path`; `None` when there is none to use.
    pub fn load(path: &Path, algo: HashAlgo) -> Option<Snapshot> {
        let file = std::fs::File::open(path).ok()?;
        let meta = file.metadata().ok()?;
        let mut bytes = vec![0; usize::try_from(meta.len()).ok()?];
        file.read_exact_at(&mut bytes, 0).ok()?;
        let (mut snapshot, whole, len, chain) = Snapshot::read(&bytes, algo)?;
        // what follows the last record that holds stays in the way of
        // another: such a file is written whole the next time
        snapshot.origin = (len == bytes.len()).then_some(Origin {
            dev: meta.dev(),
            ino: meta.ino(),
            whole: whole as u64,
            len: len as u64,
            chain,
        });
        Some(snapshot)
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
