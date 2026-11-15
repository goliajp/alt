//! A reader that opened before a compaction keeps reading after it: the
//! packs it knew are gone, the chunks are not.

use alt_store::{ChunkId, ChunkStore, Options};

use super::index_runs::chunk;

const CHUNKS: u64 = 900;

/// A store of several sealed packs, some of its chunks re-encoded as
/// deltas so a compaction has something to drop.
fn filled(dir: &std::path::Path) -> (Options, Vec<ChunkId>) {
    let opts = Options {
        seal_threshold: 20_000,
        ..Options::default()
    };
    let mut store = ChunkStore::open_with(dir, opts).unwrap();
    let ids: Vec<ChunkId> = (0..CHUNKS).map(|n| store.put(&chunk(n)).unwrap()).collect();
    store.flush().unwrap();
    store.index_tail().unwrap();
    (opts, ids)
}

fn pack_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter(|entry| {
            let name = entry.as_ref().unwrap().file_name();
            name.to_string_lossy().ends_with(".altpack")
        })
        .count()
}

#[test]
fn a_reader_opened_before_a_compaction_reads_every_chunk_after_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (opts, ids) = filled(dir.path());
    assert!(pack_count(dir.path()) > 3, "several sealed packs");

    // opened, and one early chunk read: later packs are not mapped yet
    let reader = ChunkStore::open_with(dir.path(), opts).unwrap();
    assert_eq!(reader.get(ids[0]).unwrap(), chunk(0));

    let mut writer = ChunkStore::open_with(dir.path(), opts).unwrap();
    writer.compact().unwrap();
    drop(writer);
    assert_eq!(
        pack_count(dir.path()),
        2,
        "one compacted pack and the active one"
    );

    for (n, id) in ids.iter().enumerate() {
        assert_eq!(reader.get(*id).unwrap(), chunk(n as u64), "chunk {n}");
    }
    assert_eq!(
        reader.stat(ids[500]).unwrap().orig_len as usize,
        chunk(500).len()
    );
    let mut seen = 0;
    reader.for_each_decoded(|_, _| seen += 1).unwrap();
    assert_eq!(seen, CHUNKS);
    assert_eq!(reader.check().unwrap(), CHUNKS);
}

#[test]
fn a_reader_follows_one_compaction_after_another() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (opts, ids) = filled(dir.path());
    let reader = ChunkStore::open_with(dir.path(), opts).unwrap();
    for round in 0..3u64 {
        let mut writer = ChunkStore::open_with(dir.path(), opts).unwrap();
        // new chunks, so each compaction has packs to merge
        for n in 0..300 {
            writer.put(&chunk(10_000 * (round + 1) + n)).unwrap();
        }
        writer.flush().unwrap();
        writer.compact().unwrap();
        drop(writer);
        for n in [0usize, 450, 899] {
            assert_eq!(
                reader.get(ids[n]).unwrap(),
                chunk(n as u64),
                "round {round}"
            );
        }
    }
}

#[test]
fn a_handle_that_keeps_locations_follows_every_change_to_them() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (opts, ids) = filled(dir.path());
    let mut store = ChunkStore::open_with(dir.path(), opts).unwrap();
    store.keep_reads();
    let read_all = |store: &ChunkStore| {
        for (n, id) in ids.iter().enumerate() {
            assert_eq!(store.get(*id).unwrap(), chunk(n as u64), "chunk {n}");
        }
    };
    // twice: the second pass finds every chunk where the first one did
    read_all(&store);
    read_all(&store);

    // a chunk written again as a delta: its newer record is the one found
    store.sync_from_disk().unwrap();
    let delta = store.reencode_as_delta(ids[10], ids[11]).unwrap();
    store.flush().unwrap();
    read_all(&store);
    store.index_tail().unwrap();
    read_all(&store);
    if delta {
        assert_eq!(
            store.stat(ids[10]).unwrap().encoding,
            alt_store::Encoding::Delta
        );
    }

    // every pack replaced: the locations kept so far point nowhere
    store.compact().unwrap();
    read_all(&store);
    read_all(&store);

    // new chunks, then cut back to before them
    let mark = store.checkpoint();
    let extra = store.put(&chunk(50_000)).unwrap();
    assert_eq!(store.get(extra).unwrap(), chunk(50_000));
    store.rewind(mark).unwrap();
    assert!(!store.contains(extra).unwrap());
    read_all(&store);
    assert_eq!(store.check().unwrap(), CHUNKS);
}
