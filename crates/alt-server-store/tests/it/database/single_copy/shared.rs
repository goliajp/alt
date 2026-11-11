//! The same Git object in two requests, in chunks, and uploaded again after it was
//! released: one record in the segments, nothing counted twice.
use super::*;
use alt_server_store::{ByteInput, PublicationWork};

type Ready = (PublicationWork, alt_repository_service::VerifiedGraph);

fn ready(store: &mut Store, credential: &Credential, payload: &Payload) -> Ready {
    let work = store
        .acquire_publication(credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    store.mark_ready(credential, &work, &graph).unwrap();
    (work, graph)
}
fn on_branch(mut payload: Payload, name: &str) -> Payload {
    payload.publication.refs[0].name = name.into();
    payload
}
fn segment_records(f: &mut Fixture) -> u64 {
    f.store
        .pack_view(&f.credential, "repository")
        .unwrap()
        .segments
        .objects()
}
/// nothing of the content store is stored, mapped or pinned any more
fn assert_no_content(f: &Fixture) {
    let row = f.client().query_one("SELECT (SELECT count(*) FROM alt_blobs WHERE state='stored' OR accounted),(SELECT count(*) FROM alt_objects),(SELECT count(*) FROM alt_pins),(SELECT count(*) FROM alt_blob_locations),(SELECT count(*) FROM alt_request_content)", &[]).unwrap();
    assert_eq!(
        (0..5).map(|at| row.get::<_, i64>(at)).collect::<Vec<_>>(),
        vec![0; 5]
    );
}
fn upload_one(f: &mut Fixture, payload: &Payload, at: usize) {
    let (entry, bytes) = (&payload.manifest.entries[at], &payload.contents[at]);
    let proof = f
        .store
        .bytes()
        .put(
            &payload.publication.key.scope,
            &entry.blake3,
            entry.bytes,
            limits().object_bytes,
            &mut Cursor::new(bytes),
        )
        .unwrap();
    f.store
        .register_content(&f.credential, &payload.publication.key, entry, &proof)
        .unwrap();
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn two_requests_with_the_same_new_object_publish_it_once() {
    let mut f = Fixture::new();
    let mut other = f.reopen();
    let credential = other.authenticate(&f.token).unwrap();
    let first = commit_with_file("shared-first", b"new to both", None);
    let second = on_branch(
        commit_with_file("shared-second", b"new to both", None),
        "refs/heads/other",
    );
    first.upload(&mut f.store, &f.credential);
    second.upload(&mut other, &credential);
    // the second request is leased and verified before the first one publishes
    let (work, graph) = ready(&mut other, &credential, &second);
    first.publish(&mut f.store, &f.credential);
    // the file and the tree stay for the upload still in flight; the commit went
    assert_eq!(git_rows(&f, &first).0, 2);
    assert_eq!(segment_records(&mut f), 3);
    assert_accounted(&f);
    other.publish(&credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (2, 2, 2));
    // one record for each of the four objects, although the lease predates the
    // segment that already holds two of them
    assert_eq!(segment_records(&mut f), 4);
    assert_no_content(&f);
    assert_readable(&mut f, &first);
    assert_readable(&mut f, &second);
    assert_accounted(&f);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_request_abandoned_after_another_published_its_objects_leaves_nothing() {
    let mut f = Fixture::new();
    let first = commit_with_file("abandon-first", b"new to both", None);
    let second = on_branch(
        commit_with_file("abandon-second", b"new to both", None),
        "refs/heads/other",
    );
    first.upload(&mut f.store, &f.credential);
    second.upload(&mut f.store, &f.credential);
    // the second request is leased on its own connection and never publishes
    let mut other = f.reopen();
    let credential = other.authenticate(&f.token).unwrap();
    let _leased = ready(&mut other, &credential, &second);
    first.publish(&mut f.store, &f.credential);
    assert_eq!(git_rows(&f, &second).0, 3);
    assert_accounted(&f);
    f.client()
        .execute(
            "UPDATE alt_requests SET expires_at=1 WHERE request_id='abandon-second'",
            &[],
        )
        .unwrap();
    // the first request's commit, and the three objects the second one held
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap(),
        4
    );
    assert_eq!(f.counts(), (1, 1, 1));
    assert_eq!(segment_records(&mut f), 3);
    assert_no_content(&f);
    assert_readable(&mut f, &first);
    assert_accounted(&f);
}

/// `payload` with its file declared in chunks of `size` bytes
fn in_chunks(mut payload: Payload, size: usize) -> Payload {
    payload.manifest.entries[0].chunks = payload.contents[0]
        .chunks(size)
        .enumerate()
        .map(|(at, bytes)| Chunk {
            offset: (at * size) as u64,
            bytes: bytes.len() as u64,
            blake3: alt_hash::blake3::hash(bytes).to_hex().to_string(),
        })
        .collect();
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload
}
fn upload_chunks(f: &mut Fixture, payload: &Payload, chunks: std::ops::Range<usize>) {
    let entry = &payload.manifest.entries[0];
    for chunk in &entry.chunks[chunks] {
        let start = chunk.offset as usize;
        f.store
            .upload_chunk(
                &f.credential,
                &payload.publication.key,
                &chunk.blake3,
                &mut Cursor::new(&payload.contents[0][start..start + chunk.bytes as usize]),
            )
            .unwrap();
    }
}
fn file() -> Vec<u8> {
    (0..3000u32).flat_map(|n| n.to_le_bytes()).collect()
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_git_object_uploaded_in_chunks_is_reused_without_its_chunks() {
    let mut f = Fixture::new();
    let first = in_chunks(commit_with_file("chunks-first", &file(), None), 5000);
    let begin = |f: &mut Fixture, payload: &Payload| {
        f.store
            .begin(
                &f.credential,
                &payload.publication,
                &payload.manifest,
                60_000,
            )
            .unwrap();
    };
    assert_eq!(first.manifest.entries[0].chunks.len(), 3);
    begin(&mut f, &first);
    upload_chunks(&mut f, &first, 0..3);
    f.store
        .assemble_content(
            &f.credential,
            &first.publication.key,
            &first.manifest.entries[0].blake3,
        )
        .unwrap();
    upload_one(&mut f, &first, 1);
    upload_one(&mut f, &first, 2);
    first.publish(&mut f.store, &f.credential);
    // the chunks went with the object they were assembled into
    assert_no_content(&f);
    assert_accounted(&f);
    assert_readable(&mut f, &first);

    let second = in_chunks(
        commit_with_file("chunks-second", &file(), Some(first.commit)),
        5000,
    );
    begin(&mut f, &second);
    let key = &second.publication.key;
    let granted = f.store.upload_status(&f.credential, key).unwrap();
    for chunk in &second.manifest.entries[0].chunks {
        assert!(granted.verified_chunks.contains(&chunk.blake3));
    }
    assert!(
        granted
            .verified_chunks
            .contains(&second.manifest.entries[1].blake3)
    );
    f.store
        .assemble_content(&f.credential, key, &second.manifest.entries[0].blake3)
        .unwrap();
    // nothing was reserved or stored for what the segment holds
    let row = f.client().query_one("SELECT (SELECT reserved_bytes FROM alt_repositories),(SELECT count(*) FROM alt_blobs WHERE state='stored'),(SELECT count(*) FROM alt_request_content WHERE verified),(SELECT count(*) FROM alt_request_content)", &[]).unwrap();
    assert_eq!(
        (0..4).map(|at| row.get::<_, i64>(at)).collect::<Vec<_>>(),
        vec![second.contents[2].len() as i64, 0, 5, 6]
    );
    upload_one(&mut f, &second, 2);
    second.publish(&mut f.store, &f.credential);
    assert_eq!(segment_records(&mut f), 4);
    assert_no_content(&f);
    assert_readable(&mut f, &second);
    assert_accounted(&f);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_chunked_upload_overtaken_by_a_publication_of_the_same_object_completes() {
    let mut f = Fixture::new();
    let slow = in_chunks(commit_with_file("overtaken-slow", &file(), None), 5000);
    f.store
        .begin(&f.credential, &slow.publication, &slow.manifest, 60_000)
        .unwrap();
    upload_chunks(&mut f, &slow, 0..1);
    // another request uploads and publishes the same file whole
    let fast = on_branch(
        commit_with_file("overtaken-fast", &file(), None),
        "refs/heads/other",
    );
    fast.publish_new(&mut f.store, &f.credential);
    assert_accounted(&f);
    // the slow request goes on from the one chunk it had
    let status = f
        .store
        .upload_status(&f.credential, &slow.publication.key)
        .unwrap();
    assert_eq!(
        status.verified_chunks,
        vec![slow.manifest.entries[0].chunks[0].blake3.clone()]
    );
    upload_chunks(&mut f, &slow, 1..3);
    f.store
        .assemble_content(
            &f.credential,
            &slow.publication.key,
            &slow.manifest.entries[0].blake3,
        )
        .unwrap();
    upload_one(&mut f, &slow, 1);
    upload_one(&mut f, &slow, 2);
    slow.publish(&mut f.store, &f.credential);
    assert_eq!(f.counts(), (2, 2, 2));
    assert_eq!(segment_records(&mut f), 4);
    assert_no_content(&f);
    assert_readable(&mut f, &slow);
    assert_readable(&mut f, &fast);
    assert_accounted(&f);
}

/// uploads every entry of `payload` as one byte segment and registers them together
fn upload_packed(f: &mut Fixture, payload: &Payload) {
    let mut sources: Vec<_> = payload.contents.iter().map(Cursor::new).collect();
    let mut inputs: Vec<_> = payload
        .manifest
        .entries
        .iter()
        .zip(sources.iter_mut())
        .map(|(entry, source)| ByteInput {
            digest: &entry.blake3,
            bytes: entry.bytes,
            source,
        })
        .collect();
    let proofs = f
        .store
        .persist_contents(&f.credential, &payload.publication.key, &mut inputs)
        .unwrap();
    let entries: Vec<_> = payload
        .manifest
        .entries
        .iter()
        .map(|entry| {
            (
                entry,
                proofs
                    .iter()
                    .find(|proof| proof.blake3() == entry.blake3)
                    .unwrap(),
            )
        })
        .collect();
    f.store
        .register_contents(&f.credential, &payload.publication.key, &entries)
        .unwrap();
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn content_released_and_not_yet_collected_is_uploaded_again() {
    for packed in [false, true] {
        for collect in [false, true] {
            let mut f = Fixture::new();
            let first = commit_with_file("again-first", b"uploaded twice", None);
            let begin = |f: &mut Fixture, payload: &Payload| {
                f.store
                    .begin(
                        &f.credential,
                        &payload.publication,
                        &payload.manifest,
                        60_000,
                    )
                    .unwrap();
            };
            begin(&mut f, &first);
            if packed {
                upload_packed(&mut f, &first);
            } else {
                (0..3).for_each(|at| upload_one(&mut f, &first, at));
            }
            first.publish(&mut f.store, &f.credential);
            assert_no_content(&f);
            // the released copies are still there as placeholders with their files
            let second = commit_with_file("again-second", b"uploaded twice", Some(first.commit));
            begin(&mut f, &second);
            if collect {
                // the collector runs between the release and the second upload: the
                // new request's pins keep the placeholders
                f.store
                    .collect_orphans(&f.credential, "repository", 100)
                    .unwrap();
            }
            // this client uploads everything, held by a segment or not
            if packed {
                upload_packed(&mut f, &second);
            } else {
                (0..3).for_each(|at| upload_one(&mut f, &second, at));
            }
            assert_eq!(
                git_rows(&f, &second).0,
                3,
                "packed={packed} collect={collect}"
            );
            assert_accounted(&f);
            second.publish(&mut f.store, &f.credential);
            assert_eq!(segment_records(&mut f), 4);
            assert_no_content(&f);
            assert_readable(&mut f, &second);
            assert_accounted(&f);
            f.store
                .collect_orphans(&f.credential, "repository", 100)
                .unwrap();
            assert_eq!(
                f.client()
                    .query_one(
                        "SELECT (SELECT count(*) FROM alt_blobs)+(SELECT count(*) FROM alt_segments WHERE state='stored')",
                        &[]
                    )
                    .unwrap()
                    .get::<_, i64>(0),
                0,
                "packed={packed} collect={collect}"
            );
            assert_accounted(&f);
        }
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_cloud_publication_counts_its_segment_against_the_aggregate_allowance() {
    use super::super::cloud::{claims, cloud, repository, service};
    let mut f = cloud();
    repository(&mut f);
    let payload = commit_with_file("cloud-allowance", b"small", None);
    payload.upload(&mut f.store, &f.credential);
    let (work, graph) = ready(&mut f.store, &f.credential, &payload);
    // the segment this publication writes: its three objects in object id order
    let mut objects: Vec<(ObjectId, alt_pack_segment::Kind, &Vec<u8>)> = payload
        .manifest
        .entries
        .iter()
        .zip([
            alt_pack_segment::Kind::Blob,
            alt_pack_segment::Kind::Tree,
            alt_pack_segment::Kind::Commit,
        ])
        .zip(&payload.contents)
        .map(|((entry, kind), bytes)| {
            (
                entry.git_oid.as_ref().unwrap().parse().unwrap(),
                kind,
                bytes,
            )
        })
        .collect();
    objects.sort_by_key(|(oid, _, _)| *oid);
    let none = alt_pack_segment::SegmentSet::default();
    let mut builder = alt_pack_segment::Builder::new(Vec::new(), &none, 3).unwrap();
    for (_, kind, bytes) in &objects {
        builder.add(*kind, bytes).unwrap();
    }
    let segment = builder.finish().unwrap().bytes;
    let digest = alt_server_store::cloud_credential_digest(&f.token).unwrap();
    let allow = |f: &mut Fixture, sequence: u64, bytes: u64| {
        let mut policy = service(sequence, sequence, true);
        policy.limits.stored_bytes = golia_platform_contract::Counter(bytes);
        f.store.refresh_cloud_service(&policy).unwrap();
        f.store
            .refresh_cloud_account(&digest, &claims(sequence + 1, sequence))
            .unwrap();
    };
    allow(&mut f, 3, segment - 1);
    assert!(matches!(
        f.store.publish(&f.credential, &work, &graph),
        Err(alt_server_store::Error::Limit(
            "Cloud aggregate storage quota"
        ))
    ));
    assert_eq!(f.counts(), (0, 0, 0));
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
    allow(&mut f, 5, segment);
    f.store.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
    assert_eq!(accounting(&f), (segment as i64, segment as i64));
    assert_no_content(&f);
    assert_readable(&mut f, &payload);
}
