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
