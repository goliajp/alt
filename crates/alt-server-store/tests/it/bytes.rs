use alt_remote_protocol::Scope;
use alt_server_store::ByteStore;
use std::io::{Cursor, Read};

fn scope(repo: &str) -> Scope {
    Scope {
        installation_id: "installation".into(),
        repository_instance_id: repo.into(),
        recovery_epoch: 1,
    }
}
#[test]
fn bounded_streaming_ingestion_is_durable_and_idempotent() {
    let temporary = alt_os::temp::tempdir().unwrap();
    let store = ByteStore::open(&temporary.path().join("bytes")).unwrap();
    let bytes = vec![31u8; 200_000];
    let hash = alt_hash::blake3::hash(&bytes).to_hex().to_string();
    for _ in 0..2 {
        let proof = store
            .put(
                &scope("one"),
                &hash,
                bytes.len() as u64,
                300_000,
                &mut Cursor::new(&bytes),
            )
            .unwrap();
        assert_eq!(proof.bytes(), bytes.len() as u64);
        let mut actual = Vec::new();
        store
            .verify(&scope("one"), &hash, proof.bytes())
            .unwrap()
            .read_to_end(&mut actual)
            .unwrap();
        assert_eq!(actual, bytes);
    }
    assert!(
        store
            .read(&scope("two"), &hash, bytes.len() as u64)
            .is_err()
    );
}
#[test]
fn bad_size_hash_short_read_and_limits_leave_no_content() {
    let temporary = alt_os::temp::tempdir().unwrap();
    let store = ByteStore::open(&temporary.path().join("bytes")).unwrap();
    let hash = alt_hash::blake3::hash(b"abc").to_hex().to_string();
    for (data, size, limit) in [
        (b"abc".as_slice(), 3, 2),
        (b"ab", 3, 3),
        (b"abcd", 3, 3),
        (b"xyz", 3, 3),
    ] {
        assert!(
            store
                .put(&scope("one"), &hash, size, limit, &mut Cursor::new(data))
                .is_err()
        );
        assert!(store.read(&scope("one"), &hash, 3).is_err());
    }
    assert!(
        store
            .put(&scope("one"), "../outside", 3, 3, &mut Cursor::new(b"abc"))
            .is_err()
    );
}
#[test]
fn symlink_and_corrupted_incumbent_are_not_reused() {
    use std::os::unix::fs::symlink;
    let temporary = alt_os::temp::tempdir().unwrap();
    let root = temporary.path().join("bytes");
    let store = ByteStore::open(&root).unwrap();
    let hash = alt_hash::blake3::hash(b"abc").to_hex().to_string();
    store
        .put(&scope("one"), &hash, 3, 3, &mut Cursor::new(b"abc"))
        .unwrap();
    let destination = root.join("installation/one").join(&hash);
    std::fs::write(&destination, b"bad").unwrap();
    assert!(
        store
            .put(&scope("one"), &hash, 3, 3, &mut Cursor::new(b"abc"))
            .is_err()
    );
    std::fs::remove_file(&destination).unwrap();
    let outside = temporary.path().join("outside");
    std::fs::write(&outside, b"abc").unwrap();
    symlink(&outside, &destination).unwrap();
    assert!(store.read(&scope("one"), &hash, 3).is_err());
    assert!(
        store
            .put(&scope("one"), &hash, 3, 3, &mut Cursor::new(b"abc"))
            .is_err()
    );
    assert_eq!(std::fs::read(outside).unwrap(), b"abc");
}
#[test]
fn mid_stream_io_failure_does_not_publish_partial_bytes() {
    struct Broken(bool);
    impl Read for Broken {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                return Err(std::io::Error::other("injected disk/source failure"));
            }
            self.0 = true;
            b[0] = b'a';
            Ok(1)
        }
    }
    let temporary = alt_os::temp::tempdir().unwrap();
    let root = temporary.path().join("bytes");
    let store = ByteStore::open(&root).unwrap();
    let hash = alt_hash::blake3::hash(b"abc").to_hex().to_string();
    assert!(
        store
            .put(&scope("one"), &hash, 3, 3, &mut Broken(false))
            .is_err()
    );
    assert_eq!(
        std::fs::read_dir(root.join("installation/one"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn volume_capacity_observes_the_owned_directory_and_rejects_a_replaced_symlink() {
    let temp = alt_os::temp::tempdir().unwrap();
    let path = temp.path().join("volume");
    let bytes = alt_server_store::ByteStore::open(&path).unwrap();
    let capacity = bytes.capacity().unwrap();
    assert!(capacity.total_bytes > 0);
    assert!(capacity.available_bytes <= capacity.total_bytes);
    std::fs::rename(&path, temp.path().join("original")).unwrap();
    std::os::unix::fs::symlink(temp.path().join("original"), &path).unwrap();
    assert!(bytes.capacity().is_err());
}

#[test]
fn batch_persistence_preserves_order_duplicates_empty_objects_and_retry_identity() {
    use alt_server_store::ByteInput;
    let temp = alt_os::temp::tempdir().unwrap();
    let store = ByteStore::open(&temp.path().join("bytes")).unwrap();
    let payloads = [b"abc".as_slice(), b"", b"abc", b"different"];
    let hashes: Vec<_> = payloads
        .iter()
        .map(|p| alt_hash::blake3::hash(p).to_hex().to_string())
        .collect();
    for _ in 0..2 {
        let mut sources: Vec<_> = payloads.iter().map(Cursor::new).collect();
        let mut inputs: Vec<_> = sources
            .iter_mut()
            .enumerate()
            .map(|(i, source)| ByteInput {
                digest: &hashes[i],
                bytes: payloads[i].len() as u64,
                source,
            })
            .collect();
        let proofs = store.put_batch(&scope("batch"), &mut inputs, 100).unwrap();
        assert_eq!(proofs.len(), payloads.len());
        for (i, proof) in proofs.iter().enumerate() {
            assert_eq!(proof.scope(), &scope("batch"));
            assert_eq!(proof.blake3(), hashes[i]);
            assert_eq!(proof.bytes(), payloads[i].len() as u64);
            let mut actual = Vec::new();
            store
                .verify(proof.scope(), proof.blake3(), proof.bytes())
                .unwrap()
                .read_to_end(&mut actual)
                .unwrap();
            assert_eq!(actual, payloads[i]);
        }
    }
}

#[test]
fn invalid_late_batch_input_never_installs_earlier_staged_bytes() {
    use alt_server_store::ByteInput;
    let temp = alt_os::temp::tempdir().unwrap();
    let root = temp.path().join("bytes");
    let store = ByteStore::open(&root).unwrap();
    let hash = alt_hash::blake3::hash(b"abc").to_hex().to_string();
    for data in [b"ab".as_slice(), b"abcd", b"xyz"] {
        let mut good = Cursor::new(b"abc");
        let mut bad = Cursor::new(data);
        let mut inputs = [
            ByteInput {
                digest: &hash,
                bytes: 3,
                source: &mut good,
            },
            ByteInput {
                digest: &hash,
                bytes: 3,
                source: &mut bad,
            },
        ];
        assert!(store.put_batch(&scope("batch"), &mut inputs, 3).is_err());
        assert_eq!(
            std::fs::read_dir(root.join("installation/batch"))
                .unwrap()
                .count(),
            0
        );
    }
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected source failure"))
        }
    }
    let mut good = Cursor::new(b"abc");
    let mut inputs = [
        ByteInput {
            digest: &hash,
            bytes: 3,
            source: &mut good,
        },
        ByteInput {
            digest: &hash,
            bytes: 3,
            source: &mut Broken,
        },
    ];
    assert!(store.put_batch(&scope("batch"), &mut inputs, 3).is_err());
    assert_eq!(
        std::fs::read_dir(root.join("installation/batch"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn batch_metadata_limits_are_checked_before_reading_or_creating_scope_directories() {
    use alt_server_store::{ByteInput, MAX_PUT_BYTES, MAX_PUT_INPUTS};
    struct Unread;
    impl Read for Unread {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("invalid batch must not read a source")
        }
    }
    let temp = alt_os::temp::tempdir().unwrap();
    let root = temp.path().join("bytes");
    let store = ByteStore::open(&root).unwrap();
    let hash = alt_hash::blake3::hash(b"abc").to_hex().to_string();
    for (size, limit, digest) in [
        (3, 2, hash.as_str()),
        (MAX_PUT_BYTES + 1, u64::MAX, &hash),
        (3, 3, "../bad"),
    ] {
        assert!(
            store
                .put_batch(
                    &scope("batch"),
                    &mut [ByteInput {
                        digest,
                        bytes: size,
                        source: &mut Unread
                    }],
                    limit
                )
                .is_err()
        );
    }
    let mut readers: Vec<_> = (0..MAX_PUT_INPUTS + 1).map(|_| Unread).collect();
    let mut inputs: Vec<_> = readers
        .iter_mut()
        .map(|source| ByteInput {
            digest: &hash,
            bytes: 0,
            source,
        })
        .collect();
    assert!(store.put_batch(&scope("batch"), &mut inputs, 3).is_err());
    inputs.truncate(2);
    for input in &mut inputs {
        input.bytes = MAX_PUT_BYTES;
    }
    assert!(
        store
            .put_batch(&scope("batch"), &mut inputs, u64::MAX)
            .is_err()
    );
    assert!(store.put_batch(&scope("batch"), &mut [], 3).is_err());
    assert!(
        store
            .put_batch(
                &scope("../invalid"),
                &mut [ByteInput {
                    digest: &hash,
                    bytes: 3,
                    source: &mut Unread
                }],
                3
            )
            .is_err()
    );
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
}

#[test]
fn late_corrupt_or_symlink_incumbent_returns_no_batch_proofs_and_retry_verifies_bytes() {
    use alt_server_store::ByteInput;
    use std::os::unix::fs::symlink;
    for symlink_fault in [false, true] {
        let temp = alt_os::temp::tempdir().unwrap();
        let root = temp.path().join("bytes");
        let store = ByteStore::open(&root).unwrap();
        let first = alt_hash::blake3::hash(b"first").to_hex().to_string();
        let last = alt_hash::blake3::hash(b"last").to_hex().to_string();
        store
            .put(&scope("batch"), &last, 4, 10, &mut Cursor::new(b"last"))
            .unwrap();
        let path = root.join("installation/batch").join(&last);
        if symlink_fault {
            std::fs::remove_file(&path).unwrap();
            std::fs::write(temp.path().join("outside"), b"last").unwrap();
            symlink(temp.path().join("outside"), &path).unwrap();
        } else {
            std::fs::write(&path, b"oops").unwrap();
        }
        let mut a = Cursor::new(b"first");
        let mut b = Cursor::new(b"last");
        let mut inputs = [
            ByteInput {
                digest: &first,
                bytes: 5,
                source: &mut a,
            },
            ByteInput {
                digest: &last,
                bytes: 4,
                source: &mut b,
            },
        ];
        assert!(store.put_batch(&scope("batch"), &mut inputs, 10).is_err());
        // Earlier installation may remain orphaned; it is independently checked
        // on retry and cannot become a successful partial return.
        store.verify(&scope("batch"), &first, 5).unwrap();
        std::fs::remove_file(path).unwrap();
        a.set_position(0);
        b.set_position(0);
        let mut inputs = [
            ByteInput {
                digest: &first,
                bytes: 5,
                source: &mut a,
            },
            ByteInput {
                digest: &last,
                bytes: 4,
                source: &mut b,
            },
        ];
        assert_eq!(
            store
                .put_batch(&scope("batch"), &mut inputs, 10)
                .unwrap()
                .len(),
            2
        );
    }
}

mod segments;
