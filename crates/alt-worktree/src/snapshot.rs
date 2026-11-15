//! The index as the working-tree scan wants it, kept in a file of its own
//! beside the index: every entry with the stat its working-tree file had
//! when it was last found to hold the entry's content, and every directory
//! with the id of the tree its entries make.
//!
//! The index file stays what it is, entries without stat; this file is a
//! cache and can be deleted at any time. It names the index it was built
//! from by that index's checksum, and a snapshot of an older index gives
//! what it knows to the entries that did not change.

use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};
use alt_git_index::Index;

const MAGIC: &[u8; 8] = b"ALTWTC01";
const HEADER: usize = 80;
const ENTRY: usize = 104;
const DIR: usize = 56;
const CHECKSUM: usize = 32;

/// What `lstat` says about a file, as far as it tells a changed file from
/// an unchanged one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    pub mtime: (i64, u32),
    pub ctime: (i64, u32),
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    /// `st_mode`, type and permission bits
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    path: (u32, u32),
    pub oid: ObjectId,
    pub mode: u32,
    pub flags: u16,
    /// the stat of the working-tree file when it held this content
    pub stat: Option<FileStat>,
}

impl Entry {
    pub fn stage(&self) -> u8 {
        ((self.flags >> 12) & 0b11) as u8
    }
}

/// A directory of the index: the entries `first..end` lie beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dir {
    pub first: u32,
    pub end: u32,
    /// length of the directory's path, a prefix of each of its entries' paths
    pub path_len: u32,
    /// how many directories lie beneath this one; they follow it
    pub descendants: u32,
    /// the tree these entries make, when it has been worked out
    pub tree: Option<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// checksum of the index file this was built from
    pub index_id: Vec<u8>,
    pub algo: HashAlgo,
    paths: Vec<u8>,
    pub entries: Vec<Entry>,
    /// every directory before the directories beneath it; the root first
    pub dirs: Vec<Dir>,
    /// differs from the file it was read from
    pub changed: bool,
}

impl Snapshot {
    pub fn path(&self, entry: usize) -> &[u8] {
        let (at, len) = self.entries[entry].path;
        &self.paths[at as usize..(at + len) as usize]
    }

    pub fn dir_path(&self, dir: usize) -> &[u8] {
        let d = &self.dirs[dir];
        match d.first < d.end {
            true => &self.path(d.first as usize)[..d.path_len as usize],
            false => b"",
        }
    }

    /// The entry for `path` that is not part of a conflict.
    pub fn find(&self, path: &[u8]) -> Option<usize> {
        let at = self.entries.partition_point(|e| {
            let (from, len) = e.path;
            &self.paths[from as usize..(from + len) as usize] < path
        });
        (at < self.entries.len() && self.path(at) == path && self.entries[at].stage() == 0)
            .then_some(at)
    }

    /// The directories directly in `dir`.
    pub fn child_dirs(&self, dir: usize) -> impl Iterator<Item = usize> + '_ {
        let last = dir + self.dirs[dir].descendants as usize;
        let mut next = dir + 1;
        std::iter::from_fn(move || {
            (next <= last).then(|| {
                let child = next;
                next += 1 + self.dirs[child].descendants as usize;
                child
            })
        })
    }

    /// Records what the working-tree file of `entry` looks like while it
    /// holds the entry's content.
    pub fn set_stat(&mut self, entry: usize, stat: Option<FileStat>) {
        if self.entries[entry].stat != stat {
            self.entries[entry].stat = stat;
            self.changed = true;
        }
    }

    /// The snapshot of `index`, whose file ends in the checksum `index_id`.
    /// Entries that `prior` has with the same content keep their stat, and
    /// directories whose entries are all the same keep their tree.
    pub fn of_index(
        index: &Index,
        index_id: &[u8],
        algo: HashAlgo,
        prior: Option<&Snapshot>,
    ) -> Snapshot {
        let mut order: Vec<&alt_git_index::IndexEntry> = index.entries.iter().collect();
        let key = |e: &alt_git_index::IndexEntry| (e.path.as_slice().to_vec(), e.stage());
        if !order
            .is_sorted_by(|a, b| (a.path.as_slice(), a.stage()) <= (b.path.as_slice(), b.stage()))
        {
            order.sort_by_key(|e| key(e));
        }
        let mut paths = Vec::with_capacity(order.iter().map(|e| e.path.len()).sum());
        let mut entries = Vec::with_capacity(order.len());
        // which entry of `prior` each entry is, when it is one unchanged
        let mut same: Vec<Option<u32>> = Vec::with_capacity(order.len());
        let mut at = 0;
        for e in &order {
            let path = (paths.len() as u32, e.path.len() as u32);
            paths.extend_from_slice(&e.path);
            let mut stat = None;
            let mut found = None;
            if let Some(prior) = prior {
                let want = (e.path.as_slice(), e.stage());
                while at < prior.entries.len() && (prior.path(at), prior.entries[at].stage()) < want
                {
                    at += 1;
                }
                if let Some(p) = prior.entries.get(at)
                    && (prior.path(at), p.stage()) == want
                    && p.oid == e.oid
                    && p.mode == e.mode
                    && p.flags & 0xf000 == e.flags & 0xf000
                {
                    stat = p.stat;
                    found = Some(at as u32);
                }
            }
            same.push(found);
            entries.push(Entry {
                path,
                oid: e.oid,
                mode: e.mode,
                flags: e.flags,
                stat,
            });
        }
        let mut snapshot = Snapshot {
            index_id: index_id.to_vec(),
            algo,
            paths,
            entries,
            dirs: Vec::new(),
            changed: true,
        };
        snapshot.build_dirs();
        if let Some(prior) = prior {
            snapshot.keep_trees(prior, &same);
        }
        snapshot
    }

    fn build_dirs(&mut self) {
        let mut dirs = vec![Dir {
            first: 0,
            end: self.entries.len() as u32,
            path_len: 0,
            descendants: 0,
            tree: None,
        }];
        // the directories the current entry lies in, outermost first
        let mut open: Vec<usize> = vec![0];
        for i in 0..self.entries.len() {
            let path = self.path(i);
            // leave the directories this path is not in
            while open.len() > 1 {
                let d = &dirs[*open.last().unwrap()];
                let len = d.path_len as usize;
                let prefix = &self.path(d.first as usize)[..len];
                if path.len() > len && path[len] == b'/' && &path[..len] == prefix {
                    break;
                }
                let closed = open.pop().unwrap();
                dirs[closed].end = i as u32;
                dirs[closed].descendants = (dirs.len() - closed - 1) as u32;
            }
            // enter the ones it is in beyond that
            let from = match open.len() {
                1 => 0,
                _ => dirs[*open.last().unwrap()].path_len as usize + 1,
            };
            for (slash, _) in path
                .iter()
                .enumerate()
                .skip(from)
                .filter(|(_, b)| **b == b'/')
            {
                open.push(dirs.len());
                dirs.push(Dir {
                    first: i as u32,
                    end: 0,
                    path_len: slash as u32,
                    descendants: 0,
                    tree: None,
                });
            }
        }
        let total = dirs.len();
        for closed in open.into_iter().rev() {
            dirs[closed].end = self.entries.len() as u32;
            dirs[closed].descendants = (total - closed - 1) as u32;
        }
        self.dirs = dirs;
    }

    fn keep_trees(&mut self, prior: &Snapshot, same: &[Option<u32>]) {
        // breaks[i]: how many of the entries before i do not continue the
        // run of unchanged neighbours they stand in
        let mut breaks = vec![0u32; same.len() + 1];
        for i in 0..same.len() {
            let joined = match (same[i], i.checked_sub(1).and_then(|p| same[p])) {
                (Some(now), Some(before)) => now == before + 1,
                _ => false,
            };
            breaks[i + 1] = breaks[i] + u32::from(!joined);
        }
        let known: std::collections::HashMap<(u32, u32, u32), &Dir> = prior
            .dirs
            .iter()
            .filter(|d| d.tree.is_some())
            .map(|d| ((d.first, d.end, d.path_len), d))
            .collect();
        for d in 0..self.dirs.len() {
            let (first, end) = (self.dirs[d].first as usize, self.dirs[d].end as usize);
            if first == end {
                continue;
            }
            let Some(start) = same[first] else { continue };
            if breaks[end] - breaks[first + 1] != 0 {
                continue;
            }
            let range = (start, start + (end - first) as u32, self.dirs[d].path_len);
            if let Some(old) = known.get(&range) {
                self.dirs[d].tree = old.tree;
            }
        }
    }

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
            let s = e.stat.unwrap_or(FileStat {
                mtime: (0, 0),
                ctime: (0, 0),
                dev: 0,
                ino: 0,
                size: 0,
                mode: 0,
            });
            out.extend_from_slice(&s.mtime.0.to_le_bytes());
            out.extend_from_slice(&s.ctime.0.to_le_bytes());
            out.extend_from_slice(&s.mtime.1.to_le_bytes());
            out.extend_from_slice(&s.ctime.1.to_le_bytes());
            out.extend_from_slice(&s.dev.to_le_bytes());
            out.extend_from_slice(&s.ino.to_le_bytes());
            out.extend_from_slice(&s.size.to_le_bytes());
            out.extend_from_slice(&s.mode.to_le_bytes());
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

    /// The snapshot in `bytes`; `None` when they are not one this version
    /// wrote in full.
    pub fn decode(bytes: &[u8], algo: HashAlgo) -> Option<Snapshot> {
        let body = bytes.len().checked_sub(CHECKSUM)?;
        if body < HEADER || &bytes[..8] != MAGIC || u32le(bytes, 8) != 1 {
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
        if id_len > 32 || dirs_at.checked_add(n_dirs.checked_mul(DIR)?)? != body {
            return None;
        }
        if alt_hash::blake3::hash(&bytes[..body]).as_bytes()[..] != bytes[body..] {
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
            let stat = (b[14] != 0).then(|| FileStat {
                mtime: (u64le(b, 48) as i64, u32le(b, 64)),
                ctime: (u64le(b, 56) as i64, u32le(b, 68)),
                dev: u64le(b, 72),
                ino: u64le(b, 80),
                size: u64le(b, 88),
                mode: u32le(b, 96),
            });
            entries.push(Entry {
                path,
                mode: u32le(b, 8),
                flags: u16::from_le_bytes([b[12], b[13]]),
                oid: ObjectId::from_bytes(algo, &b[16..16 + raw]).ok()?,
                stat,
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
        })
    }

    /// Reads the snapshot at `path`; `None` when there is none to use.
    pub fn load(path: &Path, algo: HashAlgo) -> Option<Snapshot> {
        Snapshot::decode(&std::fs::read(path).ok()?, algo)
    }

    /// Puts the snapshot at `path` in one step. It is not synced: a file
    /// that did not reach the disk whole fails its checksum and is ignored.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, self.encode())?;
        std::fs::rename(&tmp, path)
    }
}

fn put_oid(out: &mut Vec<u8>, oid: Option<&ObjectId>) {
    let mut raw = [0u8; 32];
    if let Some(oid) = oid {
        raw[..oid.as_bytes().len()].copy_from_slice(oid.as_bytes());
    }
    out.extend_from_slice(&raw);
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::ObjectKind;
    use alt_git_index::IndexEntry;

    fn oid(b: u8) -> ObjectId {
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &[b])
    }

    fn index(files: &[(&str, u8)]) -> Index {
        let entries = files
            .iter()
            .map(|(path, content)| IndexEntry {
                ctime: (0, 0),
                mtime: (0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                size: 0,
                oid: oid(*content),
                flags: path.len() as u16,
                extended_flags: None,
                path: (*path).into(),
            })
            .collect();
        Index {
            version: 2,
            entries,
            extensions: Vec::new(),
        }
    }

    fn stat(n: u64) -> FileStat {
        FileStat {
            mtime: (n as i64, 1),
            ctime: (n as i64, 2),
            dev: 3,
            ino: n,
            size: n,
            mode: 0o100644,
        }
    }

    #[test]
    fn directories_cover_their_entries_in_order() {
        let i = index(&[
            ("a", 1),
            ("b/c", 2),
            ("b/d/e", 3),
            ("b/d/f", 4),
            ("b/g", 5),
            ("h/i", 6),
        ]);
        let s = Snapshot::of_index(&i, b"id", HashAlgo::Sha1, None);
        let seen: Vec<(&[u8], u32, u32, u32)> = (0..s.dirs.len())
            .map(|d| {
                (
                    s.dir_path(d),
                    s.dirs[d].first,
                    s.dirs[d].end,
                    s.dirs[d].descendants,
                )
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                (&b""[..], 0, 6, 3),
                (&b"b"[..], 1, 5, 1),
                (&b"b/d"[..], 2, 4, 0),
                (&b"h"[..], 5, 6, 0),
            ]
        );
        assert_eq!(s.child_dirs(0).collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!(s.child_dirs(1).collect::<Vec<_>>(), vec![2]);
        assert_eq!(s.child_dirs(2).count(), 0);
    }

    #[test]
    fn a_sibling_whose_name_extends_a_directory_name_is_not_inside_it() {
        let i = index(&[("a/x", 1), ("a.b", 2), ("ab/y", 3)]);
        let mut sorted = i.clone();
        sorted.entries.sort_by(|a, b| a.path.cmp(&b.path));
        let s = Snapshot::of_index(&sorted, b"id", HashAlgo::Sha1, None);
        let paths: Vec<&[u8]> = (0..s.dirs.len()).map(|d| s.dir_path(d)).collect();
        assert_eq!(paths, vec![&b""[..], b"a", b"ab"]);
        assert_eq!((s.dirs[1].first, s.dirs[1].end), (1, 2));
    }

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

    #[test]
    fn unchanged_entries_keep_stat_and_untouched_directories_keep_their_tree() {
        let before = index(&[("a", 1), ("b/c", 2), ("b/d", 3), ("e/f", 4)]);
        let mut old = Snapshot::of_index(&before, b"one", HashAlgo::Sha1, None);
        for e in 0..4 {
            old.set_stat(e, Some(stat(e as u64 + 1)));
        }
        for (d, dir) in old.dirs.iter_mut().enumerate() {
            dir.tree = Some(oid(100 + d as u8));
        }
        // b/d changes content, e/g is new, e/f stays
        let after = index(&[("a", 1), ("b/c", 2), ("b/d", 9), ("e/f", 4), ("e/g", 5)]);
        let new = Snapshot::of_index(&after, b"two", HashAlgo::Sha1, Some(&old));
        let stats: Vec<_> = new.entries.iter().map(|e| e.stat).collect();
        assert_eq!(
            stats,
            vec![Some(stat(1)), Some(stat(2)), None, Some(stat(4)), None]
        );
        assert!(new.dirs.iter().all(|d| d.tree.is_none()));

        // nothing but a new top-level file: both directories keep their tree
        let grown = index(&[("a", 1), ("b/c", 2), ("b/d", 3), ("e/f", 4), ("z", 7)]);
        let new = Snapshot::of_index(&grown, b"three", HashAlgo::Sha1, Some(&old));
        let trees: Vec<_> = new.dirs.iter().map(|d| d.tree).collect();
        assert_eq!(trees, vec![None, Some(oid(101)), Some(oid(102))]);
    }
}
