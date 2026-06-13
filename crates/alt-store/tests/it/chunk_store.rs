//! Lifecycle tests: reopen visibility, crash truncation recovery, corruption
//! detection, and pack sealing/rolling.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

#[test]
fn lineage_deltas_survive_reopen_and_pack_seals() {
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        seal_threshold: 16 * 1024,
        ..Options::default()
    };

    // versions of a small file: shared body, distinct marker
    let body = random_bytes(4096, 50);
    let vs: Vec<Vec<u8>> = (0..6u64)
        .map(|i| {
            let mut v = body.clone();
            v[100..108].copy_from_slice(&i.to_le_bytes());
            v
        })
        .collect();

    let ids: Vec<alt_store::ChunkId> = {
        let mut store = ChunkStore::open_with(dir.path(), opts).unwrap();
        // bulk filler so bases and deltas land in different packs
        for i in 0..8u64 {
            store.put(&random_bytes(4096, 1000 + i)).unwrap();
        }
        let ids: Vec<alt_store::ChunkId> = vs.iter().map(|v| store.put(v).unwrap()).collect();
        for i in (0..ids.len() - 1).rev() {
            assert!(store.reencode_as_delta(ids[i], ids[i + 1]).unwrap());
        }
        store.flush().unwrap();
        ids
    };

    let store = ChunkStore::open_with(dir.path(), opts).unwrap();
    for (v, id) in vs.iter().zip(&ids) {
        assert_eq!(&store.get(*id).unwrap(), v, "delta chain after reopen");
    }
    assert_eq!(
        store.stat(ids[0]).unwrap().encoding,
        alt_store::Encoding::Delta
    );
}

#[test]
fn flipped_delta_payload_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let body = random_bytes(4096, 60);
    let mut v1 = body.clone();
    v1[7] ^= 0x55;

    let id = {
        let mut store = ChunkStore::open(dir.path()).unwrap();
        let base = store.put(&body).unwrap();
        let id = store.put(&v1).unwrap();
        assert!(store.reencode_as_delta(id, base).unwrap());
        store.flush().unwrap();
        id
    };

    // the delta record sits at the tail of the pack; flip a byte inside
    // its zstd frame (past the 32-byte base id)
    let pack = dir
        .path()
        .join(store_files(dir.path(), ".altpack")[0].clone());
    let mut bytes = fs::read(&pack).unwrap();
    let at = bytes.len() - 4;
    bytes[at] ^= 0xFF;
    fs::write(&pack, &bytes).unwrap();

    let store = ChunkStore::open(dir.path()).unwrap();
    assert!(
        matches!(store.get(id), Err(StoreError::Corrupt { .. })),
        "a flipped delta byte must surface as corruption"
    );
}

#[test]
fn reading_a_tail_detects_corruption_in_its_base() {
    // Tiered verification: a fast read no longer re-hashes every layer, only
    // the requested address. Corruption in a *base* (never hashed directly
    // when reading the tail) must still surface, caught by the tail's
    // boundary hash — and a deep scrub must localize it to the base.
    let dir = tempfile::tempdir().unwrap();
    let base_data = random_bytes(4096, 71); // incompressible -> stored raw
    let mut tail_data = base_data.clone();
    tail_data[9] ^= 0x33;

    let (base, tail) = {
        let mut store = ChunkStore::open(dir.path()).unwrap();
        let base = store.put(&base_data).unwrap();
        let tail = store.put(&tail_data).unwrap();
        assert!(store.reencode_as_delta(tail, base).unwrap());
        store.flush().unwrap();
        (base, tail)
    };

    // flip a byte inside the base's raw payload. The base is the first
    // record, so its payload starts after the file header (5) and the
    // record header (41) = offset 46.
    let pack = dir
        .path()
        .join(store_files(dir.path(), ".altpack")[0].clone());
    let mut bytes = fs::read(&pack).unwrap();
    bytes[46 + 100] ^= 0xFF;
    fs::write(&pack, &bytes).unwrap();

    let store = ChunkStore::open(dir.path()).unwrap();
    // fast read of the tail catches the corrupt base at the boundary
    assert!(
        matches!(store.get(tail), Err(StoreError::Corrupt { .. })),
        "a corrupt base must surface when reading the tail, never as data"
    );
    // and reading the base directly catches it at its own boundary
    assert!(matches!(store.get(base), Err(StoreError::Corrupt { .. })));
    // deep scrub of the tail localizes the fault to the base layer
    match store.verify_chunk(tail) {
        Err(StoreError::Corrupt { id, .. }) => assert_eq!(id, base, "deep verify localizes"),
        other => panic!("deep verify must localize the corrupt base, got {other:?}"),
    }
}

fn total_pack_bytes(dir: &Path) -> u64 {
    store_files(dir, ".altpack")
        .iter()
        .map(|n| fs::metadata(dir.join(n)).unwrap().len())
        .sum()
}

/// Builds a store where many versions delta against a base, so each version's
/// full record is superseded (dead weight). Returns (ids, datas).
fn store_with_dead_weight(dir: &Path) -> (Vec<ChunkId>, Vec<Vec<u8>>) {
    let mut ids = Vec::new();
    let mut datas = Vec::new();
    let mut store = ChunkStore::open(dir).unwrap();
    let base_data = random_bytes(8192, 200);
    let base = store.put(&base_data).unwrap();
    ids.push(base);
    datas.push(base_data.clone());
    let mut cur = base_data;
    for k in 0..30 {
        let at = k * 7 % cur.len();
        cur[at] ^= 0x5a; // small edit -> delta wins
        let id = store.put(&cur).unwrap();
        // re-encode supersedes the full record just written
        assert!(store.reencode_as_delta(id, base).unwrap());
        ids.push(id);
        datas.push(cur.clone());
    }
    store.flush().unwrap();
    (ids, datas)
}

#[test]
fn compact_reclaims_dead_weight_and_preserves_reads() {
    let dir = tempfile::tempdir().unwrap();
    let (ids, datas) = store_with_dead_weight(dir.path());
    let before = total_pack_bytes(dir.path());

    let mut store = ChunkStore::open(dir.path()).unwrap();
    let report = store.compact().unwrap();
    assert!(
        report.bytes_after < report.bytes_before,
        "compaction must reclaim dead weight: {} -> {}",
        report.bytes_before,
        report.bytes_after
    );
    assert_eq!(report.records, ids.len(), "every live record carried over");

    // every chunk still reads byte-identical right after compaction
    for (id, data) in ids.iter().zip(&datas) {
        assert_eq!(&store.get(*id).unwrap(), data, "post-compact read");
    }
    drop(store);

    // and the reclaim + correctness survive a reopen
    let store = ChunkStore::open(dir.path()).unwrap();
    for (id, data) in ids.iter().zip(&datas) {
        assert_eq!(&store.get(*id).unwrap(), data, "post-reopen read");
    }
    assert!(
        total_pack_bytes(dir.path()) < before,
        "reclaim must persist across reopen"
    );
}

#[test]
fn compact_is_idempotent_on_a_clean_store() {
    let dir = tempfile::tempdir().unwrap();
    let (ids, datas) = store_with_dead_weight(dir.path());

    let mut store = ChunkStore::open(dir.path()).unwrap();
    let first = store.compact().unwrap();
    let once = total_pack_bytes(dir.path());
    // a second compaction has no dead weight left to reclaim
    let second = store.compact().unwrap();
    assert_eq!(second.records, first.records, "same live record set");
    assert!(
        total_pack_bytes(dir.path()) <= once,
        "second compaction must not grow the store"
    );
    for (id, data) in ids.iter().zip(&datas) {
        assert_eq!(&store.get(*id).unwrap(), data);
    }
}

#[test]
fn an_orphan_compaction_tmp_pack_is_ignored() {
    // crash before the rename: a leftover `pack-<n>.altpack.tmp` must not be
    // mistaken for a real pack — the store opens cleanly from the old packs
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut store = ChunkStore::open(dir.path()).unwrap();
        let id = store.put(b"survivor").unwrap();
        store.flush().unwrap();
        id
    };
    fs::write(dir.path().join("pack-00000099.altpack.tmp"), b"garbage").unwrap();

    let store = ChunkStore::open(dir.path()).unwrap();
    assert_eq!(store.get(id).unwrap(), b"survivor");
}

/// Helper child: compacts the store at `$ALT_COMPACT_DIR`. A no-op without
/// the env var (so it passes instantly in plain `--ignored` sweeps).
#[test]
#[ignore = "helper child workload, spawned by killed_compaction_leaves_a_readable_store"]
fn compact_child_workload() {
    let Ok(dir) = std::env::var("ALT_COMPACT_DIR") else {
        return;
    };
    let mut store = ChunkStore::open(&dir).unwrap();
    store.compact().unwrap();
}

#[test]
#[ignore = "spawns a child and kill -9s it mid-compaction"]
fn killed_compaction_leaves_a_readable_store() {
    let dir = tempfile::tempdir().unwrap();
    // a store large enough that compaction outlasts the poll+kill latency
    let (ids, datas) = {
        let mut ids = Vec::new();
        let mut datas = Vec::new();
        let mut store = ChunkStore::open(dir.path()).unwrap();
        let base = random_bytes(8192, 900);
        let base_id = store.put(&base).unwrap();
        ids.push(base_id);
        datas.push(base.clone());
        // each version differs from the base at one distinct byte: unique
        // (no dedup) and a clear delta win
        for k in 0..4000usize {
            let mut v = base.clone();
            v[k] = v[k].wrapping_add(1);
            let id = store.put(&v).unwrap();
            assert!(store.reencode_as_delta(id, base_id).unwrap());
            ids.push(id);
            datas.push(v);
        }
        store.flush().unwrap();
        (ids, datas)
    };

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "chunk_store::compact_child_workload",
        ])
        .env("ALT_COMPACT_DIR", dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // kill as soon as compaction starts writing (the tmp pack appears) — or
    // shortly after, exercising a kill at an arbitrary point
    let deadline = Duration::from_secs(30);
    let start = Instant::now();
    loop {
        let has_tmp = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().ends_with(".altpack.tmp"));
        if has_tmp || start.elapsed() > Duration::from_millis(200) {
            break;
        }
        assert!(start.elapsed() < deadline, "compaction never started");
        std::thread::sleep(Duration::from_millis(1));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    // whatever point the kill landed on, the store reopens and every chunk
    // still reads back byte-identical (old packs survive until the compacted
    // pack is durable; a partial tmp is ignored)
    let store = ChunkStore::open(dir.path()).unwrap();
    for (id, data) in ids.iter().zip(&datas) {
        assert_eq!(
            &store.get(*id).unwrap(),
            data,
            "chunk lost across a killed compaction"
        );
    }
}
