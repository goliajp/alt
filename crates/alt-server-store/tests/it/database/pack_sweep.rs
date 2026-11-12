//! Removing what merges and interrupted publications leave in a repository's
//! segment directory.
use super::*;
use alt_server_store::PACK_RETIRED_GRACE_MS;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn aged(path: &Path, age: Duration) {
    std::fs::write(path, b"left behind").unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}
fn files(f: &Fixture) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(f.store.pack_directory("repository").unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    files
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_sweep_takes_row_locks_only_when_it_has_something_to_delete() {
    let mut f = Fixture::new();
    Payload::new("first", None).publish_new(&mut f.store, &f.credential);
    let live = files(&f);
    assert!(!live.is_empty());
    // retired a moment ago: its row keeps the file, however old the file is
    let retired = f.store.pack_path("repository", &"a".repeat(32)).unwrap();
    aged(&retired, Duration::from_secs(7200));
    f.client()
        .execute(
            "INSERT INTO alt_pack_segments(repository,segment_id,sequence,objects,bytes,flags,state,retired_at) VALUES('repository',$1,1,1,1,0,'retired',floor(extract(epoch FROM clock_timestamp())*1000)::bigint)",
            &[&"a".repeat(32)],
        )
        .unwrap();
    // a file without a row that a publication may be writing
    let fresh = f.store.pack_path("repository", &"c".repeat(32)).unwrap();
    aged(&fresh, Duration::ZERO);

    let mut client = f.client();
    client.batch_execute("SET lock_timeout='5s'").unwrap();
    let mut store = Store::open(
        client,
        f.store.bytes().clone(),
        f.spec.clone(),
        SigningKey::from_bytes(&f.signing_seed),
        limits(),
    )
    .unwrap();
    // another session holds the rows a locking sweep would wait for
    let mut holder = f.client();
    holder
        .batch_execute(
            "BEGIN;
             SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE;
             SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE",
        )
        .unwrap();
    let mut kept = live.clone();
    kept.extend([retired.clone(), fresh.clone()]);
    kept.sort();
    assert_eq!(
        store
            .sweep_packs("repository", PACK_RETIRED_GRACE_MS)
            .unwrap(),
        0
    );
    assert_eq!(files(&f), kept);

    // a file a crash left long ago goes under the locks: its row could be uncommitted
    let stale = f.store.pack_path("repository", &"b".repeat(32)).unwrap();
    aged(&stale, Duration::from_secs(7200));
    assert!(
        store
            .sweep_packs("repository", PACK_RETIRED_GRACE_MS)
            .is_err()
    );
    assert!(stale.exists());
    holder.batch_execute("ROLLBACK").unwrap();
    assert_eq!(
        store
            .sweep_packs("repository", PACK_RETIRED_GRACE_MS)
            .unwrap(),
        1
    );
    assert_eq!(files(&f), kept);

    // once its time is up the retired segment goes, under the locks too
    holder
        .batch_execute("BEGIN; SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE")
        .unwrap();
    assert!(store.sweep_packs("repository", 0).is_err());
    assert_eq!(files(&f), kept);
    holder.batch_execute("ROLLBACK").unwrap();
    assert_eq!(store.sweep_packs("repository", 0).unwrap(), 1);
    let mut kept = live;
    kept.push(fresh);
    kept.sort();
    assert_eq!(files(&f), kept);
    let rows: i64 = f
        .client()
        .query_one(
            "SELECT count(*) FROM alt_pack_segments WHERE state='retired'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(rows, 0);
}
