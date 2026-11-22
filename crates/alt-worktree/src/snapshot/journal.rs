//! The records of a snapshot's file, and what goes into them: a stat
//! learned for an entry, a tree worked out for a directory, and an index
//! put in place of the one the snapshot was of.
//!
//! A record is the length of what it notes, what it notes, and the BLAKE3
//! of both keyed with the checksum before it in the file: that of the
//! whole part for the first record, then that of the record before. A
//! record written over the place of another, or after another file's
//! records, is so no record of the file it is found in.

use alt_git_codec::ObjectId;

use super::file::{CHECKSUM, STAT, put_stat, stat_at, u32le};
use super::{FileStat, Replaced, Snapshot};

const NOTED_STAT: u8 = 1;
const NOTED_TREE: u8 = 2;
const NOTED_INDEX: u8 = 3;

/// The bytes a record that notes `noted` bytes takes.
pub(super) fn framed(noted: usize) -> usize {
    4 + noted + CHECKSUM
}

/// `noted` as a record after the checksum `chain`, and its own checksum.
pub(super) fn sealed(noted: &[u8], chain: &[u8; CHECKSUM]) -> (Vec<u8>, [u8; CHECKSUM]) {
    let mut record = Vec::with_capacity(framed(noted.len()));
    record.extend_from_slice(&(noted.len() as u32).to_le_bytes());
    record.extend_from_slice(noted);
    let sum = *alt_hash::blake3::keyed_hash(chain, &record).as_bytes();
    record.extend_from_slice(&sum);
    (record, sum)
}

/// What the record at the start of `bytes` notes, and its checksum, when
/// one written whole after the checksum `chain` stands there.
pub(super) fn record<'a>(
    bytes: &'a [u8],
    chain: &[u8; CHECKSUM],
) -> Option<(&'a [u8], [u8; CHECKSUM])> {
    let len = u32le(bytes.get(..4)?, 0) as usize;
    let sum = bytes.get(4 + len..framed(len))?;
    let made = alt_hash::blake3::keyed_hash(chain, &bytes[..4 + len]);
    (made.as_bytes()[..] == *sum).then(|| (&bytes[4..4 + len], *made.as_bytes()))
}

impl Snapshot {
    /// Whether what is done to this snapshot is noted for a record.
    fn notes(&mut self, more: usize) -> bool {
        // too much for records: the file is written whole
        if self
            .origin
            .as_ref()
            .is_some_and(|o| !o.takes(self.noted.len() + more))
        {
            self.origin = None;
            self.noted = Vec::new();
        }
        self.origin.is_some()
    }

    pub(super) fn note_stat(&mut self, entry: usize, stat: Option<FileStat>) {
        if self.notes(6 + STAT) {
            self.noted
                .extend_from_slice(&[NOTED_STAT, u8::from(stat.is_some())]);
            self.noted.extend_from_slice(&(entry as u32).to_le_bytes());
            put_stat(&mut self.noted, stat.as_ref());
        }
    }

    pub(crate) fn note_tree(&mut self, dir: usize, tree: &ObjectId) {
        if self.notes(5 + tree.as_bytes().len()) {
            self.noted.push(NOTED_TREE);
            self.noted.extend_from_slice(&(dir as u32).to_le_bytes());
            self.noted.extend_from_slice(tree.as_bytes());
        }
    }

    pub(super) fn note_index(&mut self, index_id: &[u8], changes: &[Replaced<'_>]) {
        let paths: usize = changes.iter().map(|c| c.path.len()).sum();
        let entries: usize = changes.iter().map(|c| c.entries.len()).sum();
        let raw = self.algo.raw_len();
        let more = 6 + index_id.len() + changes.len() * 5 + paths + entries * (6 + raw);
        if !self.notes(more) {
            return;
        }
        let out = &mut self.noted;
        out.extend_from_slice(&[NOTED_INDEX, index_id.len() as u8]);
        out.extend_from_slice(index_id);
        out.extend_from_slice(&(changes.len() as u32).to_le_bytes());
        for change in changes {
            out.extend_from_slice(&(change.path.len() as u32).to_le_bytes());
            out.extend_from_slice(change.path);
            out.push(change.entries.len() as u8);
            for (mode, flags, oid) in &change.entries {
                out.extend_from_slice(&mode.to_le_bytes());
                out.extend_from_slice(&flags.to_le_bytes());
                out.extend_from_slice(oid.as_bytes());
            }
        }
    }

    /// Does to this snapshot what `noted` says was done to the one it
    /// was noted of; `None` where it names what this one does not have.
    pub(super) fn replay(&mut self, mut noted: &[u8]) -> Option<()> {
        let raw = self.algo.raw_len();
        while let Some((&kind, rest)) = noted.split_first() {
            noted = rest;
            match kind {
                NOTED_STAT => {
                    let b = take(&mut noted, 5 + STAT)?;
                    let entry = self.entries.get_mut(u32le(b, 1) as usize)?;
                    entry.stat = (b[0] != 0).then(|| stat_at(&b[5..]));
                }
                NOTED_TREE => {
                    let b = take(&mut noted, 4 + raw)?;
                    let tree = ObjectId::from_bytes(self.algo, &b[4..]).ok()?;
                    self.dirs.get_mut(u32le(b, 0) as usize)?.tree = Some(tree);
                }
                NOTED_INDEX => {
                    let id_len = take(&mut noted, 1)?[0] as usize;
                    let index_id = take(&mut noted, id_len)?;
                    let count = u32le(take(&mut noted, 4)?, 0);
                    let mut changes = Vec::new();
                    for _ in 0..count {
                        let path_len = u32le(take(&mut noted, 4)?, 0) as usize;
                        let path = take(&mut noted, path_len)?;
                        let mut entries = Vec::new();
                        for _ in 0..take(&mut noted, 1)?[0] {
                            let b = take(&mut noted, 6 + raw)?;
                            let oid = ObjectId::from_bytes(self.algo, &b[6..]).ok()?;
                            entries.push((u32le(b, 0), u16::from_le_bytes([b[4], b[5]]), oid));
                        }
                        changes.push(Replaced { path, entries });
                    }
                    let ordered = changes.is_sorted_by(|a, b| a.path < b.path);
                    ordered.then(|| self.replace_paths(index_id, &changes))?;
                }
                _ => return None,
            }
        }
        Some(())
    }
}

/// Takes the first `len` bytes off `bytes`.
fn take<'a>(bytes: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    let (taken, rest) = bytes.split_at_checked(len)?;
    *bytes = rest;
    Some(taken)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{index, oid, stat};
    use super::*;
    use alt_git_codec::HashAlgo;

    const ALGO: HashAlgo = HashAlgo::Sha1;

    /// A snapshot read from a file of its own in `dir`.
    fn kept(dir: &std::path::Path, files: &[(&str, u8)]) -> (std::path::PathBuf, Snapshot) {
        let path = dir.join("index.scan");
        Snapshot::of_index(&index(files), &[1; 20], ALGO, None)
            .save(&path)
            .unwrap();
        let read = Snapshot::load(&path, ALGO).unwrap();
        (path, read)
    }

    fn says(a: &Snapshot) -> impl PartialEq + std::fmt::Debug + '_ {
        let entries =
            (0..a.entries.len()).map(|i| (a.path(i), &a.entries[i].oid, a.entries[i].stat));
        (&a.index_id, entries.collect::<Vec<_>>(), &a.dirs)
    }

    #[test]
    fn what_is_learned_is_added_to_the_file_and_read_back_with_it() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let (path, mut s) = kept(tmp.path(), &[("a", 1), ("b/c", 2), ("b/d", 3)]);
        let whole = std::fs::metadata(&path).unwrap().len();
        s.set_stat(1, Some(stat(5)));
        s.fill_trees();
        assert!(s.changed);
        s.save(&path).unwrap();
        assert!(!s.changed);
        let first = std::fs::metadata(&path).unwrap().len();
        assert!(
            whole < first && first < whole + 200,
            "a record was added: {first}"
        );
        // the index replaced, a path gone and one new, and a stat taken back
        let put = |content: u8| vec![(0o100644, 3, oid(content))];
        let changes = [
            Replaced {
                path: b"b/c",
                entries: Vec::new(),
            },
            Replaced {
                path: b"b/e",
                entries: put(9),
            },
        ];
        s.replace_paths(&[2; 20], &changes);
        s.set_stat(0, None);
        s.set_stat(2, Some(stat(6)));
        s.fill_trees();
        s.save(&path).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > first);
        let read = Snapshot::load(&path, ALGO).unwrap();
        assert_eq!(says(&read), says(&s));
        assert_eq!(read.index_id, vec![2; 20]);
        assert_eq!(read.path(2), b"b/e");
        assert!(read.dirs.iter().all(|d| d.tree.is_some()));
        // and the one read goes on where the one that wrote left off
        let mut read = read;
        read.set_stat(0, Some(stat(7)));
        read.save(&path).unwrap();
        assert_eq!(says(&Snapshot::load(&path, ALGO).unwrap()), says(&read));
    }

    #[test]
    fn a_record_that_did_not_reach_the_file_whole_ends_what_the_file_says() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let (path, mut s) = kept(tmp.path(), &[("a", 1), ("b", 2)]);
        s.set_stat(0, Some(stat(5)));
        s.save(&path).unwrap();
        let one = std::fs::read(&path).unwrap();
        let after_one = Snapshot::decode(&one, ALGO).unwrap();
        s.set_stat(1, Some(stat(6)));
        s.save(&path).unwrap();
        let two = std::fs::read(&path).unwrap();
        for cut in one.len() + 1..two.len() {
            let read = Snapshot::decode(&two[..cut], ALGO).unwrap();
            assert_eq!(says(&read), says(&after_one), "cut at {cut}");
        }
        let mut bad = two.clone();
        bad[one.len() + 7] ^= 1;
        assert_eq!(
            says(&Snapshot::decode(&bad, ALGO).unwrap()),
            says(&after_one)
        );
        // a file with such a rest takes no record: it is written whole
        std::fs::write(&path, &two[..two.len() - 1]).unwrap();
        let mut read = Snapshot::load(&path, ALGO).unwrap();
        read.set_stat(1, Some(stat(8)));
        read.save(&path).unwrap();
        let whole = std::fs::read(&path).unwrap();
        assert_eq!(whole, read.encode());
    }

    #[test]
    fn a_record_is_one_only_after_what_it_was_written_after() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let (path, mut s) = kept(tmp.path(), &[("a", 1), ("b", 2)]);
        let mut other = s.clone();
        s.set_stat(0, Some(stat(5)));
        s.save(&path).unwrap();
        // another process that read the file before that record: the
        // file is not what it read, and it writes the file whole
        other.set_stat(1, Some(stat(6)));
        other.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), other.encode());
        // the first one's file is gone from under it: whole again
        s.set_stat(1, Some(stat(7)));
        s.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), s.encode());
        // a record moved behind a whole part it was not written after
        let (record, _) = sealed(&[NOTED_STAT, 0, 0, 0, 0, 0], &[0; CHECKSUM]);
        let moved = [s.encode(), record].concat();
        assert_eq!(says(&Snapshot::decode(&moved, ALGO).unwrap()), says(&s));
    }

    #[test]
    fn records_stop_where_they_would_outgrow_their_share_of_the_file() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let (path, mut s) = kept(tmp.path(), &[("a", 1), ("b", 2)]);
        let whole = std::fs::metadata(&path).unwrap().len();
        let mut grew = false;
        for n in 0..1000 {
            s.set_stat(0, Some(stat(n)));
            s.save(&path).unwrap();
            let len = std::fs::metadata(&path).unwrap().len();
            grew |= len > whole;
            assert!(len <= whole + (16 << 10), "after {n} records: {len}");
        }
        assert!(grew);
        assert_eq!(says(&Snapshot::load(&path, ALGO).unwrap()), says(&s));
    }

    #[test]
    fn a_record_that_names_what_the_snapshot_does_not_have_voids_the_file() {
        let s = Snapshot::of_index(&index(&[("a", 1)]), &[1; 20], ALGO, None);
        let whole = s.encode();
        let chain: [u8; CHECKSUM] = whole[whole.len() - CHECKSUM..].try_into().unwrap();
        let mut noted = vec![NOTED_STAT, 0];
        noted.extend_from_slice(&9u32.to_le_bytes());
        noted.extend_from_slice(&[0; STAT]);
        let bytes = [whole.clone(), sealed(&noted, &chain).0].concat();
        assert!(Snapshot::decode(&bytes, ALGO).is_none());
        let bytes = [whole, sealed(&[7], &chain).0].concat();
        assert!(Snapshot::decode(&bytes, ALGO).is_none());
    }
}
