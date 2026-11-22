//! Reading a snapshot's file: the whole part through a mapping of the
//! file, which no process writes into, and the records after it in one
//! read.

use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

use alt_git_codec::HashAlgo;

use super::file::{CHECKSUM, HEADER, Laid, Origin};
use super::{Snapshot, journal};

/// What a file said.
struct Read {
    snapshot: Snapshot,
    /// the length of the whole part
    whole: usize,
    /// the length up to the end of the last record that holds
    len: usize,
    /// the checksum a record after it is chained to
    chain: [u8; CHECKSUM],
}

impl Snapshot {
    /// The snapshot `bytes` say; `None` when they do not begin with one
    /// this version wrote whole.
    pub fn decode(bytes: &[u8], algo: HashAlgo) -> Option<Snapshot> {
        let whole = Laid::of(bytes, algo)?.whole();
        let (head, tail) = bytes.split_at_checked(whole)?;
        Some(Snapshot::read(head, tail, algo)?.snapshot)
    }

    /// The snapshot that the whole part `head` and the records at the
    /// start of `tail` say.
    fn read(head: &[u8], tail: &[u8], algo: HashAlgo) -> Option<Read> {
        let laid = Laid::of(head, algo)?;
        let whole = laid.whole();
        if head.len() != whole || !laid.holds(head) {
            return None;
        }
        let mut snapshot = Snapshot::decode_whole(head, algo, &laid)?;
        let mut chain: [u8; CHECKSUM] = head[whole - CHECKSUM..whole].try_into().unwrap();
        let mut at = 0;
        while let Some((noted, sum)) = journal::record(&tail[at..], &chain) {
            // a record that holds and names what is not there is no
            // record of this snapshot: nothing of the file is believed
            snapshot.replay(noted)?;
            at += journal::framed(noted.len());
            chain = sum;
        }
        snapshot.changed = false;
        let len = whole + at;
        Some(Read {
            snapshot,
            whole,
            len,
            chain,
        })
    }

    /// Reads the snapshot at `path`; `None` when there is none to use.
    pub fn load(path: &Path, algo: HashAlgo) -> Option<Snapshot> {
        let file = std::fs::File::open(path).ok()?;
        let meta = file.metadata().ok()?;
        let size = usize::try_from(meta.len()).ok()?;
        // SAFETY: the file is that long and no process cuts one short;
        // only the whole part is read through the mapping, and no process
        // writes into the whole part of a file
        let map = unsafe { alt_os::map::Mmap::map_len(&file, size) }.ok()?;
        let whole = Laid::of(map.first(HEADER.min(size)), algo)?.whole();
        if whole > size {
            return None;
        }
        // the records are read in one piece: one may be written over
        // where two commands add theirs at the same moment
        let mut tail = vec![0; size - whole];
        file.read_exact_at(&mut tail, whole as u64).ok()?;
        let read = Snapshot::read(map.first(whole), &tail, algo)?;
        let mut snapshot = read.snapshot;
        // what follows the last record that holds stays in the way of
        // another: such a file is written whole the next time
        snapshot.origin = (read.len == size).then_some(Origin {
            dev: meta.dev(),
            ino: meta.ino(),
            whole: read.whole as u64,
            len: read.len as u64,
            chain: read.chain,
        });
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{index, stat};
    use super::*;

    const ALGO: HashAlgo = HashAlgo::Sha1;

    #[test]
    fn a_file_is_read_as_its_bytes_are_and_one_cut_short_is_none() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let path = tmp.path().join("index.scan");
        let files = index(&[("a", 1), ("b/c", 2)]);
        let mut s = Snapshot::of_index(&files, &[1; 20], ALGO, None);
        s.save(&path).unwrap();
        let whole = s.encode().len();
        let mut read = Snapshot::load(&path, ALGO).unwrap();
        assert!(!read.changed);
        read.set_stat(0, Some(stat(5)));
        read.save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > whole);
        let again = Snapshot::load(&path, ALGO).unwrap();
        assert_eq!(again.entries, read.entries);
        assert_eq!(
            again.entries,
            Snapshot::decode(&bytes, ALGO).unwrap().entries
        );
        // the whole part read through the mapping is summed
        let mut torn = bytes.clone();
        torn[HEADER + 2 * 104] ^= 0x20;
        std::fs::write(&path, &torn).unwrap();
        assert!(Snapshot::load(&path, ALGO).is_none());
        // cut short of its header or of its whole part, a file is none;
        // cut in its records, it says what it said before them
        for cut in [0, 1, HEADER - 1, HEADER, whole - 1] {
            std::fs::write(&path, &bytes[..cut]).unwrap();
            assert!(Snapshot::load(&path, ALGO).is_none(), "cut at {cut}");
        }
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(Snapshot::load(&path, ALGO).unwrap().entries, s.entries);
    }
}
