use super::scope;
use alt_byte_segment::Input;
use alt_server_store::{ByteStore, DurableSegment};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
const ID: &str = "1234567890abcdef1234567890abcdef";
fn put(store: &ByteStore, payloads: &[&[u8]]) -> alt_server_store::Result<DurableSegment> {
    let mut sources: Vec<_> = payloads.iter().map(Cursor::new).collect();
    let mut inputs: Vec<_> = sources
        .iter_mut()
        .zip(payloads)
        .map(|(source, bytes)| Input {
            digest: *blake3::hash(bytes).as_bytes(),
            bytes: bytes.len() as u64,
            source,
        })
        .collect();
    store.put_segment(&scope("segment"), ID, &mut inputs)
}
fn path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("installation/segment")
        .join(format!("segment-{ID}"))
}
#[test]
fn mixed_loose_and_segment_reads_retain_bytes_empty_members_and_independent_cursors() {
    let temp = tempfile::tempdir().unwrap();
    let store = ByteStore::open(temp.path()).unwrap();
    let data = [b"first".as_slice(), b"second", b""];
    let proof = put(&store, &data).unwrap();
    let hash = blake3::hash(data[0]);
    store
        .put(
            &scope("segment"),
            &hash.to_hex(),
            5,
            5,
            &mut Cursor::new(data[0]),
        )
        .unwrap();
    store.verify(&scope("segment"), &hash.to_hex(), 5).unwrap();
    let mut a = store.read_durable_segment(&proof, hash.as_bytes()).unwrap();
    let mut b = store
        .read_durable_segment(&proof, blake3::hash(data[1]).as_bytes())
        .unwrap();
    let mut one = [0; 1];
    a.read_exact(&mut one).unwrap();
    assert_eq!(&one, b"f");
    b.seek(SeekFrom::Start(2)).unwrap();
    b.read_exact(&mut one).unwrap();
    assert_eq!(&one, b"c");
    a.read_exact(&mut one).unwrap();
    assert_eq!(&one, b"i");
    assert!(a.seek(SeekFrom::End(1)).is_err());
    assert!(a.seek(SeekFrom::Current(-3)).is_err());
    a.seek(SeekFrom::End(0)).unwrap();
    assert_eq!(a.read(&mut one).unwrap(), 0);
    for bytes in data {
        let digest = blake3::hash(bytes);
        let location = proof.location(digest.as_bytes()).unwrap();
        let mut actual = Vec::new();
        store
            .verify_segment_member(
                proof.scope(),
                &digest.to_hex(),
                bytes.len() as u64,
                &location,
            )
            .unwrap()
            .read_to_end(&mut actual)
            .unwrap();
        assert_eq!(actual, bytes);
        assert!(
            store
                .read_segment(
                    &scope("other"),
                    &digest.to_hex(),
                    bytes.len() as u64,
                    &location
                )
                .is_err()
        );
    }
    // Repeating the exact installation verifies and reuses its immutable file.
    let retry = put(&store, &data).unwrap();
    assert_eq!(proof.index(), retry.index());
    store.check_segment_proof(&proof).unwrap();
}
#[test]
fn incumbent_conflict_corruption_and_symlink_never_get_overwritten() {
    for kind in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let store = ByteStore::open(temp.path()).unwrap();
        let proof = put(&store, &[b"original"]).unwrap();
        let destination = path(temp.path());
        match kind {
            0 => {
                assert!(put(&store, &[b"different"]).is_err());
            }
            1 => {
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&destination)
                    .unwrap();
                f.seek(SeekFrom::Start(proof.index().members()[0].offset()))
                    .unwrap();
                f.write_all(b"corrupt!").unwrap();
                assert!(put(&store, &[b"original"]).is_err());
            }
            _ => {
                std::fs::rename(&destination, temp.path().join("outside")).unwrap();
                std::os::unix::fs::symlink(temp.path().join("outside"), &destination).unwrap();
                assert!(put(&store, &[b"original"]).is_err());
            }
        }
        if kind == 0 {
            store.check_segment_proof(&proof).unwrap();
        } else {
            assert!(store.check_segment_proof(&proof).is_err());
        }
        assert!(!destination.with_extension("pending").exists());
    }
}
#[test]
fn repeated_reads_do_not_hide_missing_replaced_or_corrupt_originals() {
    for fault in 0..5 {
        let temp = tempfile::tempdir().unwrap();
        let store = ByteStore::open(temp.path()).unwrap();
        let proof = put(&store, &[b"original"]).unwrap();
        let digest = blake3::hash(b"original");
        let location = proof.location(digest.as_bytes()).unwrap();
        store
            .verify_segment_member(proof.scope(), &digest.to_hex(), 8, &location)
            .unwrap();
        let p = path(temp.path());
        match fault {
            0 => std::fs::remove_file(&p).unwrap(),
            1 => {
                std::fs::write(&p, b"truncated").unwrap();
            }
            2 | 3 => {
                let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
                f.seek(SeekFrom::Start(if fault == 2 {
                    location.offset
                } else {
                    64
                }))
                .unwrap();
                f.write_all(b"X").unwrap();
            }
            _ => {
                let old = std::fs::read(&p).unwrap();
                std::fs::remove_file(&p).unwrap();
                std::fs::write(&p, old).unwrap();
                // Identical replacement may serve current bytes, but cannot
                // inherit an earlier descriptor's durability proof.
                assert!(store.check_segment_proof(&proof).is_err());
                store
                    .verify_segment_member(proof.scope(), &digest.to_hex(), 8, &location)
                    .unwrap();
                continue;
            }
        }
        assert!(
            store
                .verify_segment_member(proof.scope(), &digest.to_hex(), 8, &location)
                .is_err()
        );
        assert!(store.check_segment_proof(&proof).is_err());
    }
}
#[test]
fn database_location_mismatch_and_truncated_open_reader_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let store = ByteStore::open(temp.path()).unwrap();
    let proof = put(&store, &[b"original"]).unwrap();
    let digest = blake3::hash(b"original");
    let location = proof.location(digest.as_bytes()).unwrap();
    for fault in 0..4 {
        let mut bad = location.clone();
        match fault {
            0 => bad.offset += 1,
            1 => bad.index_digest[0] ^= 1,
            2 => bad.segment_id = "../outside".into(),
            _ => (),
        }
        assert!(
            store
                .read_segment(
                    proof.scope(),
                    &digest.to_hex(),
                    if fault == 3 { 9 } else { 8 },
                    &bad
                )
                .is_err()
        );
    }
    let mut reader = store
        .read_segment(proof.scope(), &digest.to_hex(), 8, &location)
        .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path(temp.path()))
        .unwrap()
        .set_len(location.offset + 2)
        .unwrap();
    assert!(reader.read_to_end(&mut Vec::new()).is_err());
}
#[test]
fn pending_owner_is_exclusive_and_fenced_removal_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let store = ByteStore::open(temp.path()).unwrap();
    let proof = put(&store, &[b"original"]).unwrap();
    let p = path(temp.path());
    std::fs::write(p.with_extension("pending"), b"owned partial").unwrap();
    assert!(put(&store, &[b"original"]).is_err());
    assert_eq!(
        std::fs::read(p.with_extension("pending")).unwrap(),
        b"owned partial"
    );
    store.check_segment_proof(&proof).unwrap();
    store.remove_segment(proof.scope(), ID).unwrap();
    store.remove_segment(proof.scope(), ID).unwrap();
    assert!(!p.exists());
    assert!(!p.with_extension("pending").exists());
    assert!(store.check_segment_proof(&proof).is_err());
}
#[test]
fn bad_input_never_installs_a_segment_and_metadata_limits_do_not_read_sources() {
    let temp = tempfile::tempdir().unwrap();
    let store = ByteStore::open(temp.path()).unwrap();
    let digest = *blake3::hash(b"abc").as_bytes();
    for bytes in [b"ab".as_slice(), b"abcd", b"xyz"] {
        let mut source = Cursor::new(bytes);
        assert!(
            store
                .put_segment(
                    &scope("segment"),
                    ID,
                    &mut [Input {
                        digest,
                        bytes: 3,
                        source: &mut source
                    }]
                )
                .is_err()
        );
        assert!(!path(temp.path()).exists());
        assert!(!path(temp.path()).with_extension("pending").exists());
    }
    struct Unread;
    impl Read for Unread {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("must validate metadata first")
        }
    }
    for (id, bytes) in [("../bad", 1), (ID, alt_byte_segment::MAX_BYTES + 1)] {
        assert!(
            store
                .put_segment(
                    &scope("segment"),
                    id,
                    &mut [Input {
                        digest,
                        bytes,
                        source: &mut Unread
                    }]
                )
                .is_err()
        );
    }
    let mut a = Unread;
    let mut b = Unread;
    assert!(
        store
            .put_segment(
                &scope("segment"),
                ID,
                &mut [
                    Input {
                        digest,
                        bytes: 3,
                        source: &mut a
                    },
                    Input {
                        digest,
                        bytes: 3,
                        source: &mut b
                    }
                ]
            )
            .is_err()
    );
}
