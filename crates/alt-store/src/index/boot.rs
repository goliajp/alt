//! The boot run of the chunk index (see [`crate::runs::BootRun`]): the
//! pack records a writer left unsynced, found without the pack being
//! walked over them by every opener.
//!
//! A handle that reads the store again takes the run it was handed
//! again where it still holds. One that is to see every record as the
//! disk has it gives the run up first ([`ChunkStore::drop_boot_run`],
//! [`ChunkStore::look_again`]): the packs are then walked from where the
//! chain of runs ends, so that a writer that syncs reads every record it
//! is about to record as synced.

use alt_hash::IdMap;
use alt_os::map::Mmap;

use super::{ChunkIndex, INDEX_AT, Location};
use crate::runs::{BootRun, Guard, Key, Runs};
use crate::{ChunkId, ChunkStore, StoreError, pack};

/// What an opener hands over of the runs: the chain's by name (see
/// [`Runs::open_named`]), and the boot run it may look in.
pub(crate) type Named<'a> = (Option<&'a [(u64, u64)]>, Option<&'a BootRun>);

/// The same from what an opener was told of the store's files and of
/// its end.
pub(crate) fn named<'a>(known: Option<&'a crate::Known>, tail: &'a crate::Tail) -> Named<'a> {
    let chain = known.and_then(|known| known.chunk_runs.as_deref());
    (chain, tail.boot_run())
}

impl ChunkIndex {
    /// The range of positions of the boot run held here.
    pub fn boot_run(&self) -> Option<(u64, u64)> {
        self.runs.boot_run()
    }

    /// Moves a long enough tail into the boot run, written anew in the
    /// directory `dir` under the index directory. Run under the write
    /// lock with every record on disk known here; `guard_at` reads the
    /// pack bytes before a location. Nothing is written where the chain
    /// on disk no longer ends where this handle read it to: the tail held
    /// here may then begin past its end.
    pub fn index_boot_run(
        &mut self,
        dir: &str,
        guard_at: impl Fn(Location) -> Option<Guard>,
    ) -> Result<(), StoreError> {
        if self.tail.len() < INDEX_AT {
            return Ok(());
        }
        let guard_at = |end| guard_at(Location::at(end));
        let chain = Runs::open(&self.dir, self.base, guard_at)?;
        if chain.covered() != self.runs.chain_end() {
            return Ok(());
        }
        let tail = self.tail.iter();
        let entries: Vec<(Key, u64)> = tail.map(|(id, loc)| (id.0, loc.position())).collect();
        let guard = guard_at(self.end).ok_or(StoreError::Format("pack shorter than its index"))?;
        self.runs.extend_boot_run(dir, &entries, self.end, guard)?;
        self.tail.clear();
        self.latest.clear();
        Ok(())
    }
}

impl ChunkStore {
    /// The range of positions of the boot run this handle finds chunks
    /// in (a position is a pack and an offset in one number).
    pub fn boot_run(&self) -> Option<(u64, u64)> {
        self.index.boot_run()
    }

    /// For the holder of the write lock: no longer looks in the boot
    /// run, and walks the packs over the records it covered.
    pub fn drop_boot_run(&mut self) -> Result<(), StoreError> {
        self.tail = self.tail.clone().with_boot_run(None);
        match self.index.boot_run() {
            Some(_) => self.reload(),
            None => Ok(()),
        }
    }

    /// For a writer that leaves its appends unsynced, under the write
    /// lock with the packs read to their end: see
    /// [`ChunkIndex::index_boot_run`]. Returns the range of the boot run
    /// held then.
    pub fn index_boot_run(&mut self, dir: &str) -> Result<Option<(u64, u64)>, StoreError> {
        let packs = &self.dir;
        self.index
            .index_boot_run(dir, |loc| crate::packs::guard_at(packs, loc))?;
        Ok(self.index.boot_run())
    }

    /// The records of the packs from `from` to `end`, both where a
    /// record begins, in append order, read from the packs.
    fn walked(
        &self,
        from: Location,
        end: Location,
    ) -> Result<Vec<(ChunkId, Location)>, StoreError> {
        const SHORT: StoreError = StoreError::Format("pack shorter than its index");
        let mut found = Vec::new();
        let seqs = self.pack_seqs().into_iter();
        for seq in seqs.filter(|seq| (from.seq..=end.seq).contains(seq)) {
            let active;
            let pack = if seq == self.active.seq {
                active = unsafe { Mmap::map(&self.active.read)? };
                &active[..]
            } else {
                self.sealed_pack(seq)?
            };
            let upto = if seq == end.seq {
                end.offset as usize
            } else {
                pack.len()
            };
            let pack = pack.get(..upto).ok_or(SHORT)?;
            let (recs, read) = pack::scan_from(pack, from.offset_in(seq) as usize);
            if read != upto as u64 {
                return Err(SHORT);
            }
            let at = |(header, offset): &(pack::RecordHeader, u64)| {
                (
                    header.id,
                    Location {
                        seq,
                        offset: *offset,
                    },
                )
            };
            found.extend(recs.iter().map(at));
        }
        Ok(found)
    }

    /// Every chunk's current location, for what is to outlast this boot:
    /// the records the boot run covers are read from the packs, not taken
    /// from a file that is nowhere but in the kernel's cache.
    pub(crate) fn locations(&self) -> Result<IdMap<ChunkId, Location>, StoreError> {
        let boot = match self.index.boot_run() {
            Some((start, end)) => self.walked(Location::at(start), Location::at(end))?,
            None => Vec::new(),
        };
        Ok(self.index.all(Some(boot)))
    }

    /// Every record of the active pack with where it begins, for the idx
    /// written when the pack is sealed: read from the pack, as the idx is
    /// built again from it, and never from the index runs.
    pub(crate) fn sealing(&self) -> Result<Vec<(ChunkId, u64)>, StoreError> {
        let seq = self.active.seq;
        let from = Location {
            seq,
            offset: pack::HEADER_LEN as u64,
        };
        let end = Location {
            seq,
            offset: self.active.len,
        };
        let recs = self.walked(from, end)?;
        Ok(recs.into_iter().map(|(id, loc)| (id, loc.offset)).collect())
    }

    /// Removes every boot run of the chunk index from the disk. The
    /// caller holds the write lock.
    pub fn remove_boot_runs(&self) -> Result<(), StoreError> {
        crate::runs::remove_boot_runs(&self.dir.join("chunks.idx"), None)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::FileExt;
    use std::path::Path;

    use crate::runs::BootRun;
    use crate::{ChunkId, ChunkStore, Options, StoreError, Tail};

    /// Short enough to lie in the pack as it is.
    fn chunk(n: u32) -> Vec<u8> {
        format!("chunk number {n}").into_bytes()
    }

    fn put(store: &mut ChunkStore, chunks: std::ops::Range<u32>) -> Vec<ChunkId> {
        let ids = chunks.map(|n| store.put(&chunk(n)).unwrap()).collect();
        store.flush().unwrap();
        ids
    }

    fn opened(dir: &Path, named: Option<&BootRun>) -> Result<ChunkStore, StoreError> {
        let tail = Tail::default().with_boot_run(named.cloned());
        ChunkStore::open_known(dir, Options::default(), None, tail)
    }

    fn all_found(store: &ChunkStore, ids: &[ChunkId]) {
        for (n, id) in ids.iter().enumerate() {
            assert_eq!(store.get(*id).unwrap(), chunk(n as u32), "chunk {n}");
        }
    }

    #[test]
    fn chunks_behind_a_boot_run_are_found_with_the_pack_not_walked_until_the_store_is_read_again() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let mut store = ChunkStore::open(dir).unwrap();
        let mut ids = put(&mut store, 0..200);
        // fewer records than a run is built from
        assert_eq!(store.index_boot_run("boot").unwrap(), None);
        ids.extend(put(&mut store, 200..300));
        let (start, end) = store.index_boot_run("boot").unwrap().unwrap();
        assert_eq!(store.run_chain(), Vec::new());
        all_found(&store, &ids);
        drop(store);
        let dir_name = "boot".to_owned();
        let named = BootRun {
            dir: dir_name,
            start,
            end,
        };

        // a record the run covers gets an encoding there is none of
        let path = dir.join("pack-00000001.altpack");
        let bytes = fs::read(&path).unwrap();
        let data = chunk(150);
        let at = bytes.windows(data.len()).position(|w| w == data).unwrap() as u64 - 9;
        let pack = fs::OpenOptions::new().write(true).open(&path).unwrap();
        pack.write_all_at(&[0xff], at).unwrap();
        // whoever walks the pack stops there
        assert!(opened(dir, None).is_err());
        // with the run nobody walks it: the record is told when its
        // chunk is looked up, and every other chunk is found
        let mut store = opened(dir, Some(&named)).unwrap();
        assert_eq!(store.boot_run(), Some((start, end)));
        assert!(store.get(ids[150]).is_err());
        assert_eq!(store.get(ids[0]).unwrap(), chunk(0));
        assert_eq!(store.get(ids[299]).unwrap(), chunk(299));
        // read again the pack is walked, and the handle is as it was
        assert!(store.drop_boot_run().is_err());
        assert_eq!(store.boot_run(), Some((start, end)));
        pack.write_all_at(&[bytes[at as usize]], at).unwrap();
        store.drop_boot_run().unwrap();
        assert_eq!(store.boot_run(), None);
        all_found(&store, &ids);
        // a store loaded again looks in no boot run
        let mut store = opened(dir, Some(&named)).unwrap();
        assert_eq!(store.boot_run(), Some((start, end)));
        store.look_again().unwrap();
        assert_eq!(store.boot_run(), None);
        all_found(&store, &ids);
    }

    #[test]
    fn a_boot_run_is_written_anew_with_what_was_appended_and_goes_once_the_chain_grows() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let boot = dir.join("chunks.idx/boot");
        let mut store = ChunkStore::open(dir).unwrap();
        let mut ids = put(&mut store, 0..300);
        let first = store.index_boot_run("boot").unwrap().unwrap();
        ids.extend(put(&mut store, 300..600));
        fs::create_dir_all(dir.join("chunks.idx/another")).unwrap();
        let second = store.index_boot_run("boot").unwrap().unwrap();
        assert_eq!(second.0, first.0);
        assert!(second.1 > first.1);
        assert_eq!(fs::read_dir(&boot).unwrap().count(), 1);
        assert!(!dir.join("chunks.idx/another").exists());
        all_found(&store, &ids);
        drop(store);
        let named = |(start, end)| BootRun {
            dir: "boot".to_owned(),
            start,
            end,
        };
        // the run written first is gone: its records are walked
        let store = opened(dir, Some(&named(first))).unwrap();
        assert_eq!(store.boot_run(), None);
        all_found(&store, &ids);
        let kept = fs::read_dir(&boot).unwrap().next().unwrap().unwrap().path();
        let kept = (kept.clone(), fs::read(kept).unwrap());

        // a run of the chain over the same records ends the boot run
        let mut store = opened(dir, Some(&named(second))).unwrap();
        assert_eq!(store.boot_run(), Some(second));
        store.drop_boot_run().unwrap();
        store.index_tail().unwrap();
        assert_eq!(store.run_chain().len(), 1);
        assert!(!boot.exists());
        drop(store);
        // put back, it does not begin where the chain ends
        fs::create_dir_all(&boot).unwrap();
        fs::write(&kept.0, &kept.1).unwrap();
        let store = opened(dir, Some(&named(second))).unwrap();
        assert_eq!(store.boot_run(), None);
        all_found(&store, &ids);
    }

    /// A store whose first 300 chunks a boot run covers, the file of that
    /// run with the entries of one page gone to zeros: the run and the ids.
    fn with_a_boot_run_that_lost_a_page(dir: &Path) -> (BootRun, Vec<ChunkId>) {
        let mut store = ChunkStore::open(dir).unwrap();
        let ids = put(&mut store, 0..300);
        let (start, end) = store.index_boot_run("boot").unwrap().unwrap();
        drop(store);
        let boot = dir.join("chunks.idx/boot");
        let run = fs::read_dir(boot).unwrap().next().unwrap().unwrap().path();
        let file = fs::OpenOptions::new().write(true).open(run).unwrap();
        // a header of 56 bytes, then entries of 40
        file.write_all_at(&[0; 4096], 56 + 40 * 60).unwrap();
        let dir = "boot".to_owned();
        (BootRun { dir, start, end }, ids)
    }

    #[test]
    fn the_idx_of_a_sealed_pack_lists_what_the_pack_holds_whatever_the_boot_run_names() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let (named, mut ids) = with_a_boot_run_that_lost_a_page(dir);
        let opts = Options {
            seal_threshold: 1024,
            ..Options::default()
        };
        let tail = Tail::default().with_boot_run(Some(named.clone()));
        let mut store = ChunkStore::open_known(dir, opts, None, tail).unwrap();
        assert_eq!(store.boot_run(), Some((named.start, named.end)));
        // the pack is past the threshold: this record seals it
        ids.extend(put(&mut store, 300..301));
        assert_eq!(store.pack_seqs(), [1, 2]);
        drop(store);
        let listed = crate::idx::read(&dir.join("pack-00000001.altidx")).unwrap();
        let mut listed: Vec<ChunkId> = listed.into_iter().map(|(id, _)| id).collect();
        let mut stored = ids.clone();
        listed.sort_unstable_by_key(|id| id.0);
        stored.sort_unstable_by_key(|id| id.0);
        assert_eq!(listed, stored);
        // an opener with no run over the pack takes the idx at its word
        all_found(&opened(dir, None).unwrap(), &ids);
    }

    #[test]
    fn what_is_made_of_every_chunk_is_made_of_what_the_packs_hold_whatever_the_boot_run_names() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let (named, ids) = with_a_boot_run_that_lost_a_page(dir);
        let mut store = opened(dir, Some(&named)).unwrap();
        assert_eq!(store.boot_run(), Some((named.start, named.end)));
        let mut decoded = Vec::new();
        store.for_each_decoded(|id, _| decoded.push(id)).unwrap();
        decoded.sort_unstable_by_key(|id| id.0);
        let mut stored = ids.clone();
        stored.sort_unstable_by_key(|id| id.0);
        assert_eq!(decoded, stored);
        assert_eq!(store.compact().unwrap().records, ids.len());
        all_found(&store, &ids);
        drop(store);
        all_found(&opened(dir, None).unwrap(), &ids);
    }
}
