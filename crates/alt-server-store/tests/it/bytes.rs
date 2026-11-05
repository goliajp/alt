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
    let temporary = tempfile::tempdir().unwrap();
    let store = ByteStore::open(&temporary.path().join("bytes")).unwrap();
    let bytes = vec![31u8; 200_000];
    let hash = blake3::hash(&bytes).to_hex().to_string();
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
    let temporary = tempfile::tempdir().unwrap();
    let store = ByteStore::open(&temporary.path().join("bytes")).unwrap();
    let hash = blake3::hash(b"abc").to_hex().to_string();
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
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("bytes");
    let store = ByteStore::open(&root).unwrap();
    let hash = blake3::hash(b"abc").to_hex().to_string();
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
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("bytes");
    let store = ByteStore::open(&root).unwrap();
    let hash = blake3::hash(b"abc").to_hex().to_string();
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
