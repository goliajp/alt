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

use super::{ChunkIndex, INDEX_AT, Location};
use crate::runs::{BootRun, Guard, Key, Runs};
use crate::{ChunkStore, StoreError};

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
}
