//! Exports of Git objects the pack segments hold and the content store does not.
use super::*;

/// drops the content-store copy of everything only a ref kept: the state of objects
/// that arrived by git push
fn leave_only_segments(f: &mut Fixture) {
    f.client()
        .execute("DELETE FROM alt_pins WHERE pin_id LIKE 'ref:%'", &[])
        .unwrap();
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
}
fn stored_blobs(f: &Fixture, digest: &str) -> i64 {
    f.client()
        .query_one("SELECT count(*) FROM alt_blobs WHERE blake3=$1", &[&digest])
        .unwrap()
        .get(0)
}
fn export_pins(f: &Fixture, id: &str) -> Vec<String> {
    f.client()
        .query(
            "SELECT blake3 FROM alt_pins WHERE pin_id=$1 ORDER BY blake3",
            &[&format!("portable-export:{id}")],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}
fn read(export: &alt_server_store::PortableExport, blob: &alt_portable::Blob) -> Vec<u8> {
    let mut bytes = Vec::new();
    export.open(blob).unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}
fn intent(
    target: &Fixture,
    export: &alt_server_store::PortableExport,
) -> alt_portable::ImportIntent {
    let catalog = &export.envelope().catalog;
    alt_portable::ImportIntent {
        version: 1,
        key: RequestKey {
            scope: Scope {
                installation_id: target.spec.installation_id.clone(),
                repository_instance_id: "repository".into(),
                recovery_epoch: 1,
            },
            actor_id: "owner".into(),
            request_id: "import-segment-objects".into(),
        },
        target_public_key: target.store.receipt_public_key(),
        source: catalog.source.clone(),
        source_public_key: catalog.source_public_key.clone(),
        catalog_digest: catalog.digest(portable_limits().package).unwrap(),
        mappings: catalog
            .identities
            .iter()
            .map(|i| alt_portable::IdentityMapping {
                installation_id: i.installation_id.clone(),
                actor_id: i.actor_id.clone(),
                target_actor: Some("owner".into()),
            })
            .collect(),
    }
}
/// a commit on main whose tree holds one file with these bytes
fn payload_with_file(request: &str, raw: &[u8]) -> Payload {
    let blob = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, raw);
    let mut tree_bytes = b"100644 file\0".to_vec();
    tree_bytes.extend_from_slice(blob.as_bytes());
    let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &tree_bytes);
    let commit_bytes = format!("tree {tree}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\n{request}\n").into_bytes();
    let commit = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit_bytes);
    let mut payload = Payload::new(request, None);
    payload.contents = vec![raw.to_vec(), tree_bytes, commit_bytes];
    payload.manifest.entries = payload
        .contents
        .iter()
        .zip([
            (ContentKind::Blob, blob),
            (ContentKind::Tree, tree),
            (ContentKind::Commit, commit),
        ])
        .map(|(bytes, (kind, oid))| {
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
        })
        .collect();
    payload.commit = commit;
    payload.publication.refs[0].new = Some(commit.to_string());
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload
}
/// two commits on main, none of their objects left in the content store
fn segment_only_source() -> (Fixture, Vec<Payload>) {
    let mut source = Fixture::with_identity("source-installation", [8; 32]);
    let mut first = Payload::new("segment-first", None);
    first.publication.key.scope.installation_id = source.spec.installation_id.clone();
    first.publish_new(&mut source.store, &source.credential);
    let mut second = Payload::new("segment-second", Some(first.commit));
    second.publication.key.scope.installation_id = source.spec.installation_id.clone();
    second.publish_new(&mut source.store, &source.credential);
    leave_only_segments(&mut source);
    for payload in [&first, &second] {
        for entry in &payload.manifest.entries {
            assert_eq!(stored_blobs(&source, &entry.blake3), 0);
        }
    }
    (source, vec![first, second])
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn export_takes_git_objects_held_only_by_a_segment() {
    let (mut f, payloads) = segment_only_source();
    let scope = payloads[0].publication.key.scope.clone();
    let work = f
        .store
        .verify_portable_export(&f.credential, &scope, "segments", portable_limits())
        .unwrap();
    let export = f
        .store
        .complete_portable_export(&f.credential, work)
        .unwrap();
    let catalog = &export.envelope().catalog;
    // the empty tree is shared by both commits
    let expected: std::collections::BTreeMap<String, (ContentKind, Vec<u8>)> = payloads
        .iter()
        .flat_map(|payload| payload.manifest.entries.iter().zip(&payload.contents))
        .map(|(entry, bytes)| (entry.git_oid.clone().unwrap(), (entry.kind, bytes.clone())))
        .collect();
    assert_eq!(expected.len(), 3);
    assert_eq!(catalog.objects.len(), expected.len());
    for object in &catalog.objects {
        let (kind, bytes) = &expected[&object.oid];
        assert_eq!(object.kind, *kind);
        assert_eq!(object.payload, alt_portable::Blob::of(bytes));
        assert_eq!(&read(&export, &object.payload), bytes);
    }
    assert_eq!(catalog.blobs.len(), expected.len());
    assert_eq!(catalog.refs.len(), 1);
    assert_eq!(catalog.refs[0].oid, payloads[1].commit.to_string());
    // nothing of this export lives in the content store, so nothing is pinned there
    assert!(export_pins(&f, "segments").is_empty());
    VerifiedSnapshot::verify(
        export.envelope(),
        &scope,
        SigningKey::from_bytes(&[8; 32]).verifying_key().as_bytes(),
        &export,
        portable_limits(),
    )
    .unwrap();
    // a later handle reads the same objects
    let reopened = f
        .store
        .open_portable_export(&f.credential, &scope, "segments", portable_limits().package)
        .unwrap();
    let mut archive = Vec::new();
    alt_portable::write_archive(
        &mut archive,
        reopened.envelope(),
        &reopened,
        portable_limits().package,
    )
    .unwrap();
    for object in &catalog.objects {
        assert_eq!(read(&reopened, &object.payload), expected[&object.oid].1);
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn export_of_segment_objects_imports_into_another_instance() {
    let (mut source, payloads) = segment_only_source();
    let scope = payloads[0].publication.key.scope.clone();
    let export = source
        .store
        .prepare_portable_export(&source.credential, &scope, "migration", portable_limits())
        .unwrap();
    let mut target = Fixture::new();
    let intent = intent(&target, &export);
    stage_import(&mut target, &export, &intent);
    let work = verify_import(&mut target, &intent);
    target
        .store
        .activate_portable_import(&target.credential, &work)
        .unwrap();
    let mut seen = 0;
    for payload in &payloads {
        for (entry, bytes) in payload.manifest.entries.iter().zip(&payload.contents) {
            let (_, size, mut reader) = target
                .store
                .open_published_object(
                    &target.credential,
                    "repository",
                    entry.git_oid.as_ref().unwrap().parse().unwrap(),
                )
                .unwrap();
            let mut actual = Vec::new();
            reader.read_to_end(&mut actual).unwrap();
            assert_eq!((size, &actual), (bytes.len() as u64, bytes));
            seen += 1;
        }
    }
    assert_eq!(seen, 4);
    let view = target
        .store
        .pack_view(&target.credential, "repository")
        .unwrap();
    assert_eq!(view.refs["refs/heads/main"], payloads[1].commit);
    assert_eq!(view.segments.objects(), 3);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn bytes_that_are_a_git_blob_and_an_attachment_stay_retained_for_the_attachment() {
    let mut f = Fixture::new();
    let (task, _) = publish_task(&mut f, "shared-bytes");
    let scope = task.publication.key.scope.clone();
    let at = task
        .manifest
        .entries
        .iter()
        .position(|entry| entry.kind == ContentKind::Attachment)
        .unwrap();
    let (attachment, raw) = (task.manifest.entries[at].clone(), task.contents[at].clone());
    // the same bytes published again as a file of a commit
    let code = payload_with_file("shared-bytes-as-blob", &raw);
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &raw);
    assert_eq!(code.manifest.entries[0].blake3, attachment.blake3);
    code.publish_new(&mut f.store, &f.credential);
    leave_only_segments(&mut f);
    // the task history keeps its copy; the commit is held by its segment alone
    assert_eq!(stored_blobs(&f, &attachment.blake3), 1);
    assert_eq!(stored_blobs(&f, &code.manifest.entries[2].blake3), 0);

    let work = f
        .store
        .verify_portable_export(&f.credential, &scope, "shared", portable_limits())
        .unwrap();
    let pins = export_pins(&f, "shared");
    assert!(pins.contains(&attachment.blake3));
    assert!(!pins.contains(&code.manifest.entries[2].blake3));
    let catalog = &work_envelope(&mut f, &scope, "shared").catalog;
    let object = catalog
        .objects
        .iter()
        .find(|object| object.oid == oid.to_string())
        .expect("the blob is a Git object of the export");
    assert_eq!(object.payload.blake3, attachment.blake3);
    assert_eq!(
        catalog
            .blobs
            .iter()
            .filter(|blob| blob.blake3 == attachment.blake3)
            .count(),
        1
    );
    // without its retention the attachment could be collected under the export
    f.client()
        .execute(
            "DELETE FROM alt_pins WHERE pin_id='portable-export:shared' AND blake3=$1",
            &[&attachment.blake3],
        )
        .unwrap();
    assert!(matches!(
        f.store.complete_portable_export(&f.credential, work),
        Err(alt_server_store::Error::Precondition(
            "portable export retention changed"
        ))
    ));
    let export = f
        .store
        .prepare_portable_export(&f.credential, &scope, "shared-again", portable_limits())
        .unwrap();
    assert_eq!(read(&export, &object.payload), raw);
    VerifiedSnapshot::verify(
        export.envelope(),
        &scope,
        SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes(),
        &export,
        portable_limits(),
    )
    .unwrap();
}

/// the envelope of an export that is still being prepared
fn work_envelope(f: &mut Fixture, scope: &Scope, id: &str) -> alt_portable::Envelope {
    let row = f
        .client()
        .query_one(
            "SELECT envelope FROM alt_portable_exports WHERE repository=$1 AND export_id=$2",
            &[&scope.repository_instance_id, &id],
        )
        .unwrap();
    serde_json::from_value(row.get(0)).unwrap()
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn export_keeps_git_objects_a_task_publication_carried_outside_its_graph() {
    let mut f = Fixture::new();
    let mut payload = super::super::tasks::task_payload("loose-object");
    let raw = b"selected Git object no commit reaches".to_vec();
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &raw);
    let digest = alt_hash::blake3::hash(&raw).to_hex().to_string();
    payload.manifest.entries.push(ContentEntry {
        kind: ContentKind::Blob,
        blake3: digest.clone(),
        bytes: raw.len() as u64,
        git_oid: Some(oid.to_string()),
        chunks: vec![Chunk {
            offset: 0,
            bytes: raw.len() as u64,
            blake3: digest.clone(),
        }],
    });
    payload.contents.push(raw.clone());
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let (graph, tasks) = super::super::tasks::verified(&work, payload.commit);
    f.store
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    f.store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    let scope = payload.publication.key.scope.clone();
    let view = f.store.pack_view(&f.credential, "repository").unwrap();
    assert!(
        !view
            .segments
            .locate_any(&<[u8; 20]>::try_from(oid.as_bytes()).unwrap())
    );
    let export = f
        .store
        .prepare_portable_export(&f.credential, &scope, "loose", portable_limits())
        .unwrap();
    let object = export
        .envelope()
        .catalog
        .objects
        .iter()
        .find(|object| object.oid == oid.to_string())
        .expect("the publication's manifest names this object");
    assert_eq!(read(&export, &object.payload), raw);
    assert!(export_pins(&f, "loose").contains(&digest));
    VerifiedSnapshot::verify(
        export.envelope(),
        &scope,
        SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes(),
        &export,
        portable_limits(),
    )
    .unwrap();
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn export_lists_objects_as_the_content_store_names_them() {
    let mut f = Fixture::new();
    let (task, _) = publish_task(&mut f, "same-catalog");
    let code = payload_with_file("same-catalog-code", b"file bytes");
    code.publish_new(&mut f.store, &f.credential);
    let scope = task.publication.key.scope.clone();
    // every object still has its content-store row: both sources must agree
    let rows = f
        .client()
        .query(
            "SELECT o.oid,o.kind,o.blake3,b.bytes FROM alt_objects o JOIN alt_blobs b ON b.repository=o.repository AND b.blake3=o.blake3 ORDER BY o.oid",
            &[],
        )
        .unwrap();
    let listed: Vec<(String, String, String, i64)> = rows
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect();
    let export = f
        .store
        .prepare_portable_export(&f.credential, &scope, "same", portable_limits())
        .unwrap();
    let exported: Vec<(String, String, String, i64)> = export
        .envelope()
        .catalog
        .objects
        .iter()
        .map(|object| {
            (
                object.oid.clone(),
                serde_json::to_value(object.kind)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
                object.payload.blake3.clone(),
                object.payload.bytes as i64,
            )
        })
        .collect();
    assert_eq!(exported.len(), 5);
    assert_eq!(exported, listed);
}
