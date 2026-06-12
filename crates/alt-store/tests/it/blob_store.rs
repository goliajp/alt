//! Blob-layer lifecycle: reopen visibility, blobmap crash recovery, and
//! corruption detection.

use std::fs::{self, OpenOptions};
use std::io::Write;

use alt_store::{BlobId, BlobStore, StoreError};

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

#[test]
fn reopen_sees_flushed_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let small = b"single chunk".to_vec();
    let big = random_bytes(2 << 20, 1);

    let (a, b) = {
        let mut store = BlobStore::open(dir.path()).unwrap();
        let ids = (store.put(&small).unwrap(), store.put(&big).unwrap());
        store.flush().unwrap();
        ids
    };

    let store = BlobStore::open(dir.path()).unwrap();
    assert_eq!(store.get(a).unwrap(), small);
    assert_eq!(store.get(b).unwrap(), big);
}

#[test]
fn torn_blobmap_tail_is_dropped_and_reput_heals_cheaply() {
    let dir = tempfile::tempdir().unwrap();
    let big = random_bytes(2 << 20, 2);
    let id = {
        let mut store = BlobStore::open(dir.path()).unwrap();
        store.put(&big).unwrap()
    };

    // simulate a crash mid-append to the blobmap: a partial record
    let map_path = dir.path().join("manifests/blobmap");
    let mut f = OpenOptions::new().append(true).open(&map_path).unwrap();
    f.write_all(&[0xEE; 30]).unwrap();
    drop(f);

    let store = BlobStore::open(dir.path()).unwrap();
    assert_eq!(
        store.get(id).unwrap(),
        big,
        "torn tail must not hide records"
    );

    // chop the real record too: the blob is forgotten but its chunks are
    // not, so a re-put rebuilds only the manifest, reusing every data chunk
    drop(store);
    let size = fs::metadata(&map_path).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&map_path)
        .unwrap()
        .set_len(size - 40)
        .unwrap();

    let mut store = BlobStore::open(dir.path()).unwrap();
    assert!(matches!(store.get(id), Err(StoreError::BlobNotFound(_))));
    let before = store.counters().bytes_written;
    let again = store.put(&big).unwrap();
    assert_eq!(again, id);
    let rewritten = store.counters().bytes_written - before;
    assert!(
        rewritten < 1 << 20,
        "re-put must reuse the surviving chunks, wrote {rewritten} bytes"
    );
    assert_eq!(store.get(id).unwrap(), big);
}

#[test]
fn corrupt_blobmap_record_mid_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = BlobStore::open(dir.path()).unwrap();
        store.put(&random_bytes(2 << 20, 3)).unwrap();
        store.put(&random_bytes(2 << 20, 4)).unwrap();
    }

    let map_path = dir.path().join("manifests/blobmap");
    let mut bytes = fs::read(&map_path).unwrap();
    // flip a byte inside the FIRST record (not the last): real corruption,
    // not a torn tail — must refuse to open, never silently drop records
    bytes[5 + 10] ^= 0xFF;
    fs::write(&map_path, &bytes).unwrap();

    let err = match BlobStore::open(dir.path()) {
        Ok(_) => panic!("corrupt blobmap must refuse to open"),
        Err(e) => e,
    };
    assert!(matches!(err, StoreError::Format(_)), "got {err:?}");
}

#[test]
fn flipped_data_chunk_surfaces_through_blob_get() {
    let dir = tempfile::tempdir().unwrap();
    let big = random_bytes(2 << 20, 5);
    let id = {
        let mut store = BlobStore::open(dir.path()).unwrap();
        let id = store.put(&big).unwrap();
        store.flush().unwrap();
        id
    };

    // flip one byte deep inside the first pack's payload area
    let pack_path = dir.path().join("pack-00000001.altpack");
    let mut bytes = fs::read(&pack_path).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    fs::write(&pack_path, &bytes).unwrap();

    let store = BlobStore::open(dir.path()).unwrap();
    assert!(
        matches!(store.get(id), Err(StoreError::Corrupt { .. })),
        "blob reads must surface chunk corruption"
    );
}

#[test]
fn blob_ids_are_plain_blake3_of_content() {
    // the address must be exactly blake3(content) — the bridge map.alt
    // (M2/S4) depends on this being stable and chunking-independent
    let data = random_bytes(300_000, 6);
    assert_eq!(BlobId::of(&data).0, blake3_reference(&data));
}

fn blake3_reference(data: &[u8]) -> [u8; 32] {
    // independent path through the public crate API (non-rayon)
    let mut hasher = blake3::Hasher::new();
    hasher.update(data);
    *hasher.finalize().as_bytes()
}
