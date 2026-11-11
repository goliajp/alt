//! Repositories published to before Git objects left the content store.
use super::*;

/// Puts the content store back into the shape publications left before Git objects
/// moved to the segments: a stored copy of each object with its mapping and a
/// permanent pin of the ref that reaches it.
fn as_published_before(f: &mut Fixture, published: &Payload, extra: &[(ContentEntry, Vec<u8>)]) {
    let mut again = Payload::new("old-shape-upload", Some(published.commit));
    again
        .manifest
        .entries
        .extend(published.manifest.entries.iter().cloned());
    again.contents.extend(published.contents.iter().cloned());
    for (entry, bytes) in extra {
        again.manifest.entries.push(entry.clone());
        again.contents.push(bytes.clone());
    }
    again.publication.content_manifest = again.manifest.id(limits()).unwrap();
    again.upload(&mut f.store, &f.credential);
    let mut client = f.client();
    for entry in &published.manifest.entries {
        client.execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES('repository','ref:refs/heads/main',$1,NULL)", &[&entry.blake3]).unwrap();
    }
    for (entry, _) in extra {
        client.execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES('repository','task-bundle:before',$1,NULL)", &[&entry.blake3]).unwrap();
    }
    client
        .execute(
            "UPDATE alt_requests SET expires_at=1 WHERE request_id='old-shape-upload'",
            &[],
        )
        .unwrap();
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn content_published_before_git_objects_left_the_content_store_keeps_working() {
    let mut f = Fixture::new();
    let old = commit_with_file("single-old", b"old file", None);
    old.publish_new(&mut f.store, &f.credential);
    let raw = b"an object an old task publication carried outside its graph".to_vec();
    let loose = git_entry(
        ContentKind::Blob,
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &raw),
        &raw,
    );
    as_published_before(&mut f, &old, &[(loose.clone(), raw.clone())]);
    assert_eq!(git_rows(&f, &old), (3, 3, 3));
    let pins = |f: &Fixture| -> i64 {
        f.client()
            .query_one(
                "SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'ref:%' OR pin_id LIKE 'task-bundle:%'",
                &[],
            )
            .unwrap()
            .get(0)
    };
    assert_eq!(pins(&f), 4);
    assert_readable(&mut f, &old);

    // a publication on top reuses the tree and the file without uploading them
    let next = commit_with_file("single-next", b"old file", Some(old.commit));
    f.store
        .begin(&f.credential, &next.publication, &next.manifest, 60_000)
        .unwrap();
    let proof = f
        .store
        .bytes()
        .put(
            &next.publication.key.scope,
            &next.manifest.entries[2].blake3,
            next.manifest.entries[2].bytes,
            limits().object_bytes,
            &mut Cursor::new(&next.contents[2]),
        )
        .unwrap();
    f.store
        .register_content(
            &f.credential,
            &next.publication.key,
            &next.manifest.entries[2],
            &proof,
        )
        .unwrap();
    next.publish(&mut f.store, &f.credential);
    assert_readable(&mut f, &old);
    assert_readable(&mut f, &next);
    // what was retained before stays as it was; the new commit did not stay
    assert_eq!(git_rows(&f, &old), (3, 3, 3));
    assert_eq!(pins(&f), 4);
    assert_eq!(
        f.client()
            .query_one(
                "SELECT count(*) FROM alt_blobs WHERE state='stored' AND blake3=$1",
                &[&next.manifest.entries[2].blake3]
            )
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap(),
        1
    );
    assert_eq!(git_rows(&f, &old), (3, 3, 3));

    let snapshot = f
        .store
        .read_snapshot(&f.credential, "repository", 60_000, 1000)
        .unwrap();
    assert_eq!(snapshot.objects().count(), 4);

    // an export takes the old object no segment holds from the content store, and
    // retains only that
    let scope = old.publication.key.scope.clone();
    let export = f
        .store
        .prepare_portable_export(
            &f.credential,
            &scope,
            "old-shape",
            super::super::portable::portable_limits(),
        )
        .unwrap();
    let catalog = &export.envelope().catalog;
    assert_eq!(catalog.objects.len(), 5);
    assert!(
        catalog
            .objects
            .iter()
            .any(|object| Some(&object.oid) == loose.git_oid.as_ref())
    );
    let export_pins: Vec<String> = f
        .client()
        .query(
            "SELECT blake3 FROM alt_pins WHERE pin_id='portable-export:old-shape'",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(export_pins, vec![loose.blake3.clone()]);
    alt_portable::VerifiedSnapshot::verify(
        export.envelope(),
        &scope,
        SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes(),
        &export,
        super::super::portable::portable_limits(),
    )
    .unwrap();
}
