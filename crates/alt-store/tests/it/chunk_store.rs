//! Lifecycle tests: reopen visibility, crash truncation recovery, corruption
//! detection, and pack sealing/rolling.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use alt_store::{ChunkId, ChunkStore, Options, StoreError};

/// Deterministic pseudo-random bytes (splitmix64 stream).
fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = seed;
    while out.len() < len {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(len);
    out
}

fn store_files(dir: &Path, suffix: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(suffix))
        .collect();
    names.sort();
    names
}

#[test]
fn reopen_sees_flushed_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let cases: Vec<Vec<u8>> = vec![
        b"small".to_vec(),
        vec![7u8; 50_000],
        random_bytes(200_000, 1),
    ];

    let ids: Vec<ChunkId> = {
        let mut store = ChunkStore::open(dir.path()).unwrap();
        let ids = cases.iter().map(|c| store.put(c).unwrap()).collect();
        store.flush().unwrap();
        ids
    };

    let store = ChunkStore::open(dir.path()).unwrap();
    assert_eq!(store.len(), cases.len());
    for (case, id) in cases.iter().zip(&ids) {
        assert_eq!(&store.get(*id).unwrap(), case);
    }
}

#[test]
fn torn_tail_is_dropped_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let a_data = random_bytes(10_000, 2);
    let b_data = random_bytes(10_000, 3);
    let (a, b) = {
        let mut store = ChunkStore::open(dir.path()).unwrap();
        (store.put(&a_data).unwrap(), store.put(&b_data).unwrap())
    };

    // simulate a crash mid-append: garbage that parses as a partial record
    let pack = dir
        .path()
        .join(store_files(dir.path(), ".altpack")[0].clone());
    let mut f = OpenOptions::new().append(true).open(&pack).unwrap();
    f.write_all(&[0xCD; 20]).unwrap();
    drop(f);

    let mut store = ChunkStore::open(dir.path()).unwrap();
    assert_eq!(store.len(), 2, "torn tail must not hide complete records");
    assert_eq!(store.get(a).unwrap(), a_data);
    assert_eq!(store.get(b).unwrap(), b_data);

    // the truncated pack must accept appends again, durably
    let c_data = random_bytes(5_000, 4);
    let c = store.put(&c_data).unwrap();
    assert_eq!(store.get(c).unwrap(), c_data);
    drop(store);
    let store = ChunkStore::open(dir.path()).unwrap();
    assert_eq!(store.get(c).unwrap(), c_data);
}

#[test]
fn truncation_mid_record_drops_only_the_tail_record() {
    let dir = tempfile::tempdir().unwrap();
    let a_data = random_bytes(10_000, 5);
    let b_data = random_bytes(10_000, 6);
    let (a, b) = {
        let mut store = ChunkStore::open(dir.path()).unwrap();
        (store.put(&a_data).unwrap(), store.put(&b_data).unwrap())
    };

    let pack = dir
        .path()
        .join(store_files(dir.path(), ".altpack")[0].clone());
    let size = fs::metadata(&pack).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&pack)
        .unwrap()
        .set_len(size - 10)
        .unwrap();

    let store = ChunkStore::open(dir.path()).unwrap();
    assert_eq!(store.len(), 1);
    assert_eq!(store.get(a).unwrap(), a_data);
    assert!(matches!(store.get(b), Err(StoreError::NotFound(_))));
}

#[test]
fn flipped_payload_byte_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    // incompressible -> stored raw -> the flip hits the re-hash check
    let data = random_bytes(100, 7);
    let id = {
        let mut store = ChunkStore::open(dir.path()).unwrap();
        store.put(&data).unwrap()
    };

    let pack = dir
        .path()
        .join(store_files(dir.path(), ".altpack")[0].clone());
    let mut bytes = fs::read(&pack).unwrap();
    // file header (5) + record header (41) + a few bytes into the payload
    bytes[5 + 41 + 3] ^= 0xFF;
    fs::write(&pack, &bytes).unwrap();

    let store = ChunkStore::open(dir.path()).unwrap();
    assert!(
        matches!(store.get(id), Err(StoreError::Corrupt { .. })),
        "a flipped payload byte must surface as corruption, never as data"
    );
}

#[test]
fn flipped_compressed_payload_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    // compressible -> stored zstd -> the flip breaks decode or the re-hash
    let data: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let id = {
        let mut store = ChunkStore::open(dir.path()).unwrap();
        store.put(&data).unwrap()
    };

    let pack = dir
        .path()
        .join(store_files(dir.path(), ".altpack")[0].clone());
    let mut bytes = fs::read(&pack).unwrap();
    let mid = 5 + 41 + (bytes.len() - 5 - 41) / 2;
    bytes[mid] ^= 0xFF;
    fs::write(&pack, &bytes).unwrap();

    let store = ChunkStore::open(dir.path()).unwrap();
    assert!(matches!(store.get(id), Err(StoreError::Corrupt { .. })));
}

#[test]
fn sealing_rolls_to_new_packs_and_all_chunks_stay_readable() {
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        seal_threshold: 16 * 1024,
        ..Options::default()
    };

    let chunks: Vec<Vec<u8>> = (0..10).map(|i| random_bytes(4096, 100 + i)).collect();
    let ids: Vec<ChunkId> = {
        let mut store = ChunkStore::open_with(dir.path(), opts).unwrap();
        let ids: Vec<ChunkId> = chunks.iter().map(|c| store.put(c).unwrap()).collect();
        // sealed-pack chunks must be readable in the same session
        for (chunk, id) in chunks.iter().zip(&ids) {
            assert_eq!(&store.get(*id).unwrap(), chunk);
        }
        store.flush().unwrap();
        ids
    };

    let packs = store_files(dir.path(), ".altpack");
    let idxs = store_files(dir.path(), ".altidx");
    assert!(
        packs.len() >= 2,
        "seal threshold must roll packs: {packs:?}"
    );
    assert_eq!(idxs.len(), packs.len() - 1, "every sealed pack has an idx");

    let store = ChunkStore::open_with(dir.path(), opts).unwrap();
    assert_eq!(store.len(), chunks.len());
    for (chunk, id) in chunks.iter().zip(&ids) {
        assert_eq!(&store.get(*id).unwrap(), chunk);
    }
}

#[test]
fn missing_idx_is_rebuilt_from_the_pack() {
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        seal_threshold: 16 * 1024,
        ..Options::default()
    };

    let chunks: Vec<Vec<u8>> = (0..10).map(|i| random_bytes(4096, 200 + i)).collect();
    let ids: Vec<ChunkId> = {
        let mut store = ChunkStore::open_with(dir.path(), opts).unwrap();
        chunks.iter().map(|c| store.put(c).unwrap()).collect()
    };

    let idxs = store_files(dir.path(), ".altidx");
    assert!(!idxs.is_empty());
    for idx in &idxs {
        fs::remove_file(dir.path().join(idx)).unwrap();
    }

    let store = ChunkStore::open_with(dir.path(), opts).unwrap();
    for (chunk, id) in chunks.iter().zip(&ids) {
        assert_eq!(&store.get(*id).unwrap(), chunk);
    }
    // the cache got repaired on open
    assert_eq!(store_files(dir.path(), ".altidx").len(), idxs.len());
}
