//! The chunk store and the blob store open the same with or without their
//! index runs, and the chunk index follows what happens to the packs.

use std::fs;
use std::path::Path;

use alt_store::reclog::INDEX_AT;
use alt_store::{BlobStore, ChunkId, ChunkStore, Options};

use super::index_runs::chunk;

fn fill_chunks(dir: &Path, opts: Options, range: std::ops::Range<u64>) -> Vec<ChunkId> {
    let mut store = ChunkStore::open_with(dir, opts).unwrap();
    let ids = range.map(|n| store.put(&chunk(n)).unwrap()).collect();
    store.flush().unwrap();
    store.index_tail().unwrap();
    ids
}

#[test]
fn chunk_store_reads_the_same_with_and_without_its_index() {
    let dir = alt_os::temp::tempdir().unwrap();
    let total = INDEX_AT as u64 * 2;
    let mut ids = fill_chunks(dir.path(), Options::default(), 0..total);
    let idx = dir.path().join("chunks.idx");
    assert!(idx.is_dir());
    ids.extend(fill_chunks(
        dir.path(),
        Options::default(),
        total..total + 5,
    ));

    let check = |store: &ChunkStore| {
        assert_eq!(store.len() as u64, total + 5);
        for n in [0, 1, total - 1, total, total + 4] {
            assert_eq!(store.get(ids[n as usize]).unwrap(), chunk(n));
        }
        assert!(!store.contains(ChunkId::of(b"absent")).unwrap());
    };
    check(&ChunkStore::open(dir.path()).unwrap());
    fs::remove_dir_all(&idx).unwrap();
    check(&ChunkStore::open(dir.path()).unwrap());
}

#[test]
fn chunk_index_follows_seals_rewinds_deltas_and_compaction() {
    let dir = alt_os::temp::tempdir().unwrap();
    // small packs: the index has to span several of them
    let opts = Options {
        seal_threshold: 20_000,
        ..Options::default()
    };
    let total = INDEX_AT as u64 * 3;
    let ids = fill_chunks(dir.path(), opts, 0..total);
    {
        let store = ChunkStore::open_with(dir.path(), opts).unwrap();
        for n in (0..total).step_by(37) {
            assert_eq!(store.get(ids[n as usize]).unwrap(), chunk(n));
        }
    }

    // rewind: everything appended after the checkpoint goes, index included
    {
        let store = ChunkStore::open_with(dir.path(), opts).unwrap();
        let ckpt = store.checkpoint();
        let small = Options {
            seal_threshold: u64::MAX,
            ..opts
        };
        drop(store);
        let mut store = ChunkStore::open_with(dir.path(), small).unwrap();
        let extra: Vec<ChunkId> = (total..total + INDEX_AT as u64 + 3)
            .map(|n| store.put(&chunk(n)).unwrap())
            .collect();
        store.flush().unwrap();
        store.index_tail().unwrap();
        store.rewind(ckpt).unwrap();
        assert!(!store.contains(extra[0]).unwrap());
        assert_eq!(store.get(ids[5]).unwrap(), chunk(5));
    }
    let mut store = ChunkStore::open_with(dir.path(), opts).unwrap();
    assert_eq!(store.len() as u64, total);

    // a delta re-encoding supersedes the indexed location of its chunk
    let a = store.put(&vec![1u8; 9_000]).unwrap();
    let mut b_data = vec![1u8; 9_000];
    b_data[4_000] = 2;
    let b = store.put(&b_data).unwrap();
    store.flush().unwrap();
    store.reencode_as_delta(b, a).unwrap();
    store.flush().unwrap();
    drop(store);
    let mut store = ChunkStore::open_with(dir.path(), opts).unwrap();
    assert_eq!(store.get(b).unwrap(), b_data);

    store.compact().unwrap();
    assert_eq!(store.get(b).unwrap(), b_data);
    drop(store);
    let store = ChunkStore::open_with(dir.path(), opts).unwrap();
    assert_eq!(store.len() as u64, total + 2);
    assert_eq!(
        store.get(ids[total as usize - 1]).unwrap(),
        chunk(total - 1)
    );
    assert_eq!(store.get(b).unwrap(), b_data);
}

#[test]
fn blob_store_reopens_from_its_index() {
    let dir = alt_os::temp::tempdir().unwrap();
    let blobs: Vec<Vec<u8>> = (0..INDEX_AT as u64 * 2).map(chunk).collect();
    let ids: Vec<_> = {
        let mut store = BlobStore::open(dir.path()).unwrap();
        let ids = blobs
            .iter()
            .map(|b| store.put(b).unwrap())
            .collect::<Vec<_>>();
        store.flush().unwrap();
        store.index_tail().unwrap();
        ids
    };
    let store = BlobStore::open(dir.path()).unwrap();
    for (blob, id) in blobs.iter().zip(&ids).step_by(41) {
        assert_eq!(&store.get(*id).unwrap(), blob);
    }
}
