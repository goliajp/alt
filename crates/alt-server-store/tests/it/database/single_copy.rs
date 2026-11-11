//! A published Git object lives in the pack segments alone: what a native publication
//! uploads leaves the content store when it is published.
use super::*;
use std::io::Read;

/// a commit on main whose tree holds one file with these bytes
fn commit_with_file(request: &str, raw: &[u8], parent: Option<ObjectId>) -> Payload {
    let blob = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, raw);
    let mut tree_bytes = b"100644 file\0".to_vec();
    tree_bytes.extend_from_slice(blob.as_bytes());
    let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &tree_bytes);
    let parents = parent.map(|p| format!("parent {p}\n")).unwrap_or_default();
    let commit_bytes = format!("tree {tree}\n{parents}author A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\n{request}\n").into_bytes();
    let commit = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit_bytes);
    let mut payload = Payload::new(request, parent);
    payload.contents = vec![raw.to_vec(), tree_bytes, commit_bytes];
    payload.manifest.entries = payload
        .contents
        .iter()
        .zip([
            (ContentKind::Blob, blob),
            (ContentKind::Tree, tree),
            (ContentKind::Commit, commit),
        ])
        .map(|(bytes, (kind, oid))| git_entry(kind, oid, bytes))
        .collect();
    payload.commit = commit;
    payload.publication.refs[0].new = Some(commit.to_string());
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload
}
fn git_entry(kind: ContentKind, oid: ObjectId, bytes: &[u8]) -> ContentEntry {
    let digest = alt_hash::blake3::hash(bytes).to_hex().to_string();
    ContentEntry {
        kind,
        blake3: digest.clone(),
        bytes: bytes.len() as u64,
        git_oid: Some(oid.to_string()),
        chunks: vec![Chunk {
            offset: 0,
            bytes: bytes.len() as u64,
            blake3: digest,
        }],
    }
}
fn publish_task(f: &mut Fixture, payload: &Payload) {
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let (graph, tasks) = super::tasks::verified(&work, payload.commit);
    f.store
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    f.store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
}
/// rows the content store has for the Git entries of a payload: stored content,
/// object mappings, pins
fn git_rows(f: &Fixture, payload: &Payload) -> (i64, i64, i64) {
    let digests: Vec<&str> = payload
        .manifest
        .entries
        .iter()
        .filter(|entry| entry.git_oid.is_some())
        .map(|entry| entry.blake3.as_str())
        .collect();
    let row = f.client().query_one("SELECT (SELECT count(*) FROM alt_blobs WHERE state='stored' AND blake3=ANY($1)),(SELECT count(*) FROM alt_objects WHERE blake3=ANY($1)),(SELECT count(*) FROM alt_pins WHERE blake3=ANY($1))", &[&digests]).unwrap();
    (row.get(0), row.get(1), row.get(2))
}
/// the bytes counted against the quota, and what they should be: the live pack
/// segments and the content the content store still retains
fn accounting(f: &Fixture) -> (i64, i64) {
    let row = f.client().query_one("SELECT (SELECT stored_bytes FROM alt_repositories WHERE id='repository'),(SELECT COALESCE(sum(bytes),0) FROM alt_pack_segments WHERE state='live')::bigint+(SELECT COALESCE(sum(bytes),0) FROM alt_blobs b WHERE EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=b.repository AND p.blake3=b.blake3))::bigint", &[]).unwrap();
    (row.get(0), row.get(1))
}
fn assert_accounted(f: &Fixture) {
    let (counted, held) = accounting(f);
    assert_eq!(counted, held);
    assert!(counted > 0);
}
fn assert_readable(f: &mut Fixture, payload: &Payload) {
    let scope = &payload.publication.key.scope;
    for (entry, bytes) in payload.manifest.entries.iter().zip(&payload.contents) {
        if let Some(oid) = &entry.git_oid {
            let (_, size, mut reader) = f
                .store
                .open_published_object(&f.credential, "repository", oid.parse().unwrap())
                .unwrap();
            let mut actual = Vec::new();
            reader.read_to_end(&mut actual).unwrap();
            assert_eq!((size, &actual), (bytes.len() as u64, bytes));
        }
        let (size, mut reader) = f
            .store
            .open_published_content(&f.credential, scope, &entry.blake3)
            .unwrap();
        let mut actual = Vec::new();
        reader.read_to_end(&mut actual).unwrap();
        assert_eq!((size, &actual), (bytes.len() as u64, bytes));
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_native_publication_leaves_its_git_objects_in_the_segments_alone() {
    let mut f = Fixture::new();
    let first = commit_with_file("single-first", b"file content", None);
    first.publish_new(&mut f.store, &f.credential);
    assert_eq!(git_rows(&f, &first), (0, 0, 0));
    assert_eq!(
        f.store
            .pack_view(&f.credential, "repository")
            .unwrap()
            .segments
            .objects(),
        3
    );
    assert_readable(&mut f, &first);
    assert_accounted(&f);
    // the released copies are placeholders until the collector removes them
    let rows = |f: &Fixture| -> i64 {
        f.client()
            .query_one("SELECT count(*) FROM alt_blobs", &[])
            .unwrap()
            .get(0)
    };
    assert_eq!(rows(&f), 3);
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap(),
        3
    );
    assert_eq!(rows(&f), 0);
    assert_readable(&mut f, &first);
    assert_accounted(&f);

    // a second commit on the same tree: only the commit is uploaded
    let second = commit_with_file("single-second", b"file content", Some(first.commit));
    let key = &second.publication.key;
    f.store
        .begin(&f.credential, &second.publication, &second.manifest, 60_000)
        .unwrap();
    let granted = f.store.upload_status(&f.credential, key).unwrap();
    for held in &second.manifest.entries[..2] {
        assert!(granted.verified_chunks.contains(&held.blake3));
        f.store
            .assemble_content(&f.credential, key, &held.blake3)
            .unwrap();
    }
    assert!(
        !granted
            .verified_chunks
            .contains(&second.manifest.entries[2].blake3)
    );
    assert_eq!(
        f.client()
            .query_one("SELECT reserved_bytes FROM alt_repositories", &[])
            .unwrap()
            .get::<_, i64>(0),
        second.contents[2].len() as i64
    );
    let proof = f
        .store
        .bytes()
        .put(
            &key.scope,
            &second.manifest.entries[2].blake3,
            second.manifest.entries[2].bytes,
            limits().object_bytes,
            &mut Cursor::new(&second.contents[2]),
        )
        .unwrap();
    f.store
        .register_content(&f.credential, key, &second.manifest.entries[2], &proof)
        .unwrap();
    second.publish(&mut f.store, &f.credential);
    assert_eq!(git_rows(&f, &second), (0, 0, 0));
    assert_eq!(
        f.store
            .pack_view(&f.credential, "repository")
            .unwrap()
            .segments
            .objects(),
        4
    );
    assert_readable(&mut f, &first);
    assert_readable(&mut f, &second);
    assert_accounted(&f);
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    assert_eq!(rows(&f), 0);
    assert_accounted(&f);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_task_publication_publishes_selected_git_objects_no_commit_reaches() {
    let mut f = Fixture::new();
    let mut payload = super::tasks::task_payload("single-loose");
    let raw = b"selected Git object no commit reaches".to_vec();
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &raw);
    payload
        .manifest
        .entries
        .push(git_entry(ContentKind::Blob, oid, &raw));
    payload.contents.push(raw);
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    publish_task(&mut f, &payload);
    assert_eq!(git_rows(&f, &payload), (0, 0, 0));
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    // a receiver takes every entry of the manifest by its digest
    assert_readable(&mut f, &payload);
    let retained: i64 = f
        .client()
        .query_one(
            "SELECT count(*) FROM alt_blobs WHERE state='stored' AND accounted",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(retained, 5);
    assert_accounted(&f);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn bytes_that_are_an_attachment_and_a_git_blob_are_kept_once_and_packed() {
    let mut f = Fixture::new();
    let task = super::tasks::task_payload("single-shared");
    publish_task(&mut f, &task);
    let at = task
        .manifest
        .entries
        .iter()
        .position(|entry| entry.kind == ContentKind::Attachment)
        .unwrap();
    let (attachment, raw) = (&task.manifest.entries[at], &task.contents[at]);
    assert_accounted(&f);
    let code = commit_with_file("single-shared-file", raw, None);
    assert_eq!(code.manifest.entries[0].blake3, attachment.blake3);
    code.publish_new(&mut f.store, &f.credential);
    let row = f
        .client()
        .query_one(
            "SELECT count(*),bool_and(state='stored' AND accounted) FROM alt_blobs WHERE blake3=$1",
            &[&attachment.blake3],
        )
        .unwrap();
    assert_eq!((row.get::<_, i64>(0), row.get::<_, bool>(1)), (1, true));
    // the tree and the commit went; the shared bytes stay for the attachment
    let gone: i64 = f
        .client()
        .query_one(
            "SELECT count(*) FROM alt_blobs WHERE state='stored' AND blake3=ANY($1)",
            &[&vec![
                code.manifest.entries[1].blake3.as_str(),
                code.manifest.entries[2].blake3.as_str(),
            ]],
        )
        .unwrap()
        .get(0);
    assert_eq!(gone, 0);
    assert_readable(&mut f, &code);
    assert_readable(&mut f, &task);
    assert_accounted(&f);
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    assert_readable(&mut f, &task);
    assert_accounted(&f);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_publication_whose_segment_exceeds_the_quota_fails_and_leaves_nothing() {
    let mut f = Fixture::new();
    let payload = commit_with_file("single-quota", b"small", None);
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 30_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    // room for what was uploaded, not for the segment with its index
    f.client()
        .execute(
            "UPDATE alt_repositories SET quota_bytes=stored_bytes+100",
            &[],
        )
        .unwrap();
    let before = accounting(&f).0;
    assert!(matches!(
        f.store.publish(&f.credential, &work, &graph),
        Err(alt_server_store::Error::Limit("repository quota"))
    ));
    assert_eq!(f.counts(), (0, 0, 0));
    assert_eq!(accounting(&f).0, before);
    assert_eq!(git_rows(&f, &payload).0, 3);
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_pack_segments", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        std::fs::read_dir(f.store.pack_directory("repository").unwrap())
            .unwrap()
            .count(),
        0
    );
    f.client()
        .execute("UPDATE alt_repositories SET quota_bytes=100000000", &[])
        .unwrap();
    f.store.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
    assert_readable(&mut f, &payload);
    assert_accounted(&f);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_selected_git_object_that_refers_to_nothing_published_rejects_the_request() {
    let mut f = Fixture::new();
    let mut payload = Payload::new("single-dangling", None);
    // a tree naming a file that is neither selected nor published
    let missing = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"never uploaded");
    let mut tree = b"100644 file\0".to_vec();
    tree.extend_from_slice(missing.as_bytes());
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &tree);
    payload
        .manifest
        .entries
        .push(git_entry(ContentKind::Tree, oid, &tree));
    payload.contents.push(tree);
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 30_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    assert!(matches!(
        f.store.publish(&f.credential, &work, &graph),
        Err(alt_server_store::Error::Integrity(
            "native Git objects do not form a valid segment"
        ))
    ));
    assert_eq!(f.counts(), (0, 0, 0));
    assert!(matches!(
        f.store.query(&f.credential, &payload.publication.key).unwrap(),
        RequestObservation::Recorded { result, .. } if matches!(*result, RequestState::Rejected { .. })
    ));
    assert_eq!(
        std::fs::read_dir(f.store.pack_directory("repository").unwrap())
            .unwrap()
            .count(),
        0
    );
}

mod before;
