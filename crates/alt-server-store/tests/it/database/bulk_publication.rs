use super::*;

fn bulk_limits() -> Limits {
    Limits {
        manifest_entries: 5000,
        graph_objects: 5000,
        tree_paths: 5000,
        ..limits()
    }
}
fn payload() -> Payload {
    let mut objects = Vec::new();
    let mut tree = Vec::new();
    for index in 0..1100 {
        let raw = if index == 0 {
            Vec::new()
        } else {
            format!("payload {index}\n").into_bytes()
        };
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &raw);
        tree.extend_from_slice(format!("100644 f{index:04}\0").as_bytes());
        tree.extend_from_slice(oid.as_bytes());
        objects.push((ContentKind::Blob, oid, raw));
    }
    // Equal canonical bytes with two Git kinds must retain both mappings while
    // their storage accounting and pins are deduplicated.
    let empty_tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
    tree.extend_from_slice(b"40000 zempty\0");
    tree.extend_from_slice(empty_tree.as_bytes());
    objects.push((ContentKind::Tree, empty_tree, Vec::new()));
    let tree_oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &tree);
    objects.push((ContentKind::Tree, tree_oid, tree));
    let commit_bytes = format!("tree {tree_oid}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nbulk\n").into_bytes();
    let commit = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit_bytes);
    objects.push((ContentKind::Commit, commit, commit_bytes));
    let mut p = Payload::new("bulk", None);
    p.commit = commit;
    p.publication.refs[0].new = Some(commit.to_string());
    p.manifest.entries = objects
        .iter()
        .map(|(kind, oid, raw)| {
            let digest = alt_hash::blake3::hash(raw).to_hex().to_string();
            ContentEntry {
                kind: *kind,
                blake3: digest.clone(),
                bytes: raw.len() as u64,
                git_oid: Some(oid.to_string()),
                chunks: if raw.is_empty() {
                    vec![]
                } else {
                    vec![Chunk {
                        offset: 0,
                        bytes: raw.len() as u64,
                        blake3: digest,
                    }]
                },
            }
        })
        .collect();
    p.contents = objects.into_iter().map(|(_, _, raw)| raw).collect();
    p.publication.content_manifest = p.manifest.id(bulk_limits()).unwrap();
    p
}
fn larger_store(f: &Fixture) -> Store {
    Store::open(
        f.client(),
        f.store.bytes().clone(),
        f.spec.clone(),
        SigningKey::from_bytes(&f.signing_seed),
        bulk_limits(),
    )
    .unwrap()
}

#[test]
#[ignore = "requires isolated PostgreSQL; exact ref closures and atomic rollback after a later SQL batch"]
fn batched_refs_keep_exact_closures_and_roll_back_late_failures() {
    use std::collections::BTreeMap;
    let mut f = Fixture::new();
    f.store = Store::open(
        f.client(),
        f.store.bytes().clone(),
        f.spec.clone(),
        SigningKey::from_bytes(&f.signing_seed),
        Limits {
            ref_updates: 2000,
            ..limits()
        },
    )
    .unwrap();
    let mut first = Payload::new("many-ref-first", None);
    first.publication.refs = (0..1100)
        .map(|i| RefUpdate {
            name: format!("refs/heads/r{i:04}"),
            expected: None,
            new: Some(first.commit.to_string()),
            force: false,
        })
        .collect();
    first.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &first.publication.key, 30_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    let mut client = f.client();
    client.execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES('repository','independent-owner',$1,NULL)", &[&first.manifest.entries[0].blake3]).unwrap();
    let pins_before: i64 = client
        .query_one("SELECT count(*) FROM alt_pins", &[])
        .unwrap()
        .get(0);
    // Sequence effects survive rollback, proving that the first batch really
    // ran before the later ref failed, rather than merely failing admission.
    client.batch_execute("CREATE SEQUENCE observed_refs;
        CREATE FUNCTION fail_late_ref() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN PERFORM nextval('observed_refs');
          IF NEW.name='refs/heads/r1050' THEN RAISE EXCEPTION 'late ref failure'; END IF;
          RETURN NEW;
        END $$;
        CREATE TRIGGER fail_late_ref BEFORE INSERT ON alt_refs FOR EACH ROW EXECUTE FUNCTION fail_late_ref();").unwrap();
    assert!(f.store.publish(&f.credential, &work, &graph).is_err());
    assert!(
        client
            .query_one("SELECT last_value FROM observed_refs", &[])
            .unwrap()
            .get::<_, i64>(0)
            > 1024
    );
    assert_eq!(f.counts(), (0, 0, 0));
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM alt_pins", &[])
            .unwrap()
            .get::<_, i64>(0),
        pins_before
    );
    assert!(
        matches!(f.store.query(&f.credential, &first.publication.key).unwrap(), RequestObservation::Recorded { result, .. } if matches!(*result, RequestState::Ready { .. }))
    );
    client.batch_execute("DROP TRIGGER fail_late_ref ON alt_refs; DROP FUNCTION fail_late_ref(); DROP SEQUENCE observed_refs;").unwrap();
    f.store.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1100, 1, 1));

    let mut second = Payload::new("many-ref-second", Some(first.commit));
    second.publication.refs = (0..1100)
        .filter(|i| i % 3 != 2)
        .map(|i| RefUpdate {
            name: format!("refs/heads/r{i:04}"),
            expected: Some(first.commit.to_string()),
            new: (i % 3 == 1).then(|| second.commit.to_string()),
            force: false,
        })
        .chain((0..400).map(|i| RefUpdate {
            name: format!("refs/heads/new{i:04}"),
            expected: None,
            new: Some(second.commit.to_string()),
            force: false,
        }))
        .collect();
    assert!(second.publication.refs.len() > 1024);
    second.upload(&mut f.store, &f.credential);
    second.publish(&mut f.store, &f.credential);
    let mut expected: BTreeMap<String, String> = first
        .publication
        .refs
        .iter()
        .map(|r| (r.name.clone(), first.commit.to_string()))
        .collect();
    for r in &second.publication.refs {
        if let Some(oid) = &r.new {
            expected.insert(r.name.clone(), oid.clone());
        } else {
            expected.remove(&r.name);
        }
    }
    let actual: BTreeMap<String, String> = client
        .query("SELECT name,oid FROM alt_refs", &[])
        .unwrap()
        .into_iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(actual, expected);
    // refs record no retention: what they reach is held by the pack segments
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'ref:%'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    for payload in [&first, &second] {
        for entry in &payload.manifest.entries {
            f.store
                .open_published_object(
                    &f.credential,
                    "repository",
                    entry.git_oid.as_ref().unwrap().parse().unwrap(),
                )
                .unwrap();
        }
    }
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM alt_pins WHERE pin_id='independent-owner'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(f.counts(), (expected.len() as i64, 2, 2));
}

#[test]
#[ignore = "requires isolated PostgreSQL; checks no-op, changed and missing pins across multiple SQL batches"]
fn acquire_preserves_equal_pins_and_refreshes_changed_or_missing_pins() {
    let mut f = Fixture::new();
    f.store = larger_store(&f);
    let p = payload();
    p.upload(&mut f.store, &f.credential);
    let pin = format!(
        "upload:{}",
        alt_hash::blake3::hash(&serde_json::to_vec(&p.publication.key).unwrap()).to_hex()
    );
    let mut client = f.client();
    let tuples =
        |client: &mut Client| -> std::collections::BTreeMap<String, (String, Option<i64>)> {
            client.query("SELECT blake3,ctid::text,expires_at FROM alt_pins WHERE pin_id=$1 ORDER BY blake3", &[&pin]).unwrap().into_iter()
            .map(|row| (row.get(0), (row.get(1), row.get(2)))).collect()
        };
    let before = tuples(&mut client);
    assert!(before.len() > 1024);
    f.store
        .acquire_publication(&f.credential, &p.publication.key, 30_000, 5000)
        .unwrap();
    assert_eq!(tuples(&mut client), before);

    // Simulate the first worker's lease ending before the next acquisition;
    // dropping its handle alone deliberately does not release that lease.
    client
        .execute("UPDATE alt_requests SET worker_lease_until=0", &[])
        .unwrap();

    let changed = before.keys().next().unwrap();
    let missing = before.keys().nth(1025).unwrap();
    // A permanent pin independently makes the missing upload member visible
    // to graph acquisition. It must get this request's exact finite pin again.
    client.execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES('repository','published',$1,NULL)", &[missing]).unwrap();
    client
        .execute(
            "DELETE FROM alt_pins WHERE pin_id=$1 AND blake3=$2",
            &[&pin, missing],
        )
        .unwrap();
    client
        .execute(
            "UPDATE alt_pins SET expires_at=NULL WHERE pin_id=$1 AND blake3=$2",
            &[&pin, changed],
        )
        .unwrap();
    f.store
        .acquire_publication(&f.credential, &p.publication.key, 30_000, 5000)
        .unwrap();
    let after = tuples(&mut client);
    assert_eq!(after.len(), before.len());
    for (digest, (tuple, expiry)) in &before {
        assert_eq!(&after[digest].1, expiry);
        if digest != changed && digest != missing {
            assert_eq!(&after[digest].0, tuple);
        }
    }
    assert_ne!(after[changed].0, before[changed].0);
    assert_ne!(after[missing].0, before[missing].0);
}

#[test]
#[ignore = "requires isolated PostgreSQL; validates whole-transaction rollback across SQL batches"]
fn bulk_begin_collision_rolls_back_earlier_batches_and_reservation() {
    let mut f = Fixture::new();
    f.store = larger_store(&f);
    let p = payload();
    let last = p.manifest.entries.iter().max_by_key(|e| &e.blake3).unwrap();
    f.client().execute("INSERT INTO alt_blobs(repository,blake3,bytes,state,created_at) VALUES('repository',$1,$2,'deleting',0)", &[&last.blake3,&(last.bytes as i64)]).unwrap();
    assert!(matches!(
        f.store
            .begin(&f.credential, &p.publication, &p.manifest, 60_000),
        Err(alt_server_store::Error::Precondition(_))
    ));
    let row = f.client().query_one("SELECT (SELECT count(*) FROM alt_blobs),(SELECT count(*) FROM alt_requests),(SELECT count(*) FROM alt_pins),(SELECT reserved_bytes FROM alt_repositories)", &[]).unwrap();
    for (index, expected) in [1i64, 0, 0, 0].into_iter().enumerate() {
        assert_eq!(row.get::<_, i64>(index), expected);
    }
    f.client()
        .execute("DELETE FROM alt_blobs WHERE blake3=$1", &[&last.blake3])
        .unwrap();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let unique: std::collections::BTreeMap<_, _> = p
        .manifest
        .entries
        .iter()
        .map(|e| (&e.blake3, e.bytes))
        .collect();
    assert_eq!(
        f.client()
            .query_one("SELECT reserved_bytes FROM alt_repositories", &[])
            .unwrap()
            .get::<_, i64>(0),
        unique.values().sum::<u64>() as i64
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL; selected mappings and graph pins must reject missing rows across SQL batches"]
fn bulk_publication_checks_every_mapping_and_pin_then_reads_exact_bytes() {
    let mut f = Fixture::new();
    f.store = larger_store(&f);
    let p = payload();
    p.upload(&mut f.store, &f.credential);
    let selected = &p.manifest.entries[1099];
    let oid = selected.git_oid.as_ref().unwrap();
    f.client()
        .execute(
            "DELETE FROM alt_objects WHERE repository='repository' AND oid=$1",
            &[oid],
        )
        .unwrap();
    assert!(matches!(
        f.store
            .acquire_publication(&f.credential, &p.publication.key, 30_000, 5000),
        Err(alt_server_store::Error::Precondition(
            "Git mapping not uploaded"
        ))
    ));
    f.client()
        .execute(
            "INSERT INTO alt_objects(repository,oid,kind,blake3) VALUES('repository',$1,'blob',$2)",
            &[oid, &selected.blake3],
        )
        .unwrap();
    let work = f
        .store
        .acquire_publication(&f.credential, &p.publication.key, 30_000, 5000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        GraphLimits {
            objects: 5000,
            paths: 5000,
            ..graph_limits()
        },
    )
    .unwrap();
    let pin = format!(
        "upload:{}",
        alt_hash::blake3::hash(&serde_json::to_vec(&p.publication.key).unwrap()).to_hex()
    );
    f.client()
        .execute(
            "DELETE FROM alt_pins WHERE repository='repository' AND pin_id=$1 AND blake3=$2",
            &[&pin, &selected.blake3],
        )
        .unwrap();
    assert!(matches!(
        f.store.mark_ready(&f.credential, &work, &graph),
        Err(alt_server_store::Error::Precondition(
            "object pin expired or fenced"
        ))
    ));
    // Another request's permanent pin cannot stand in for this worker's pin.
    f.client().execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES('repository','unrelated-live',$1,NULL)", &[&selected.blake3]).unwrap();
    assert!(matches!(
        f.store.mark_ready(&f.credential, &work, &graph),
        Err(alt_server_store::Error::Precondition(
            "object pin expired or fenced"
        ))
    ));
    f.client().execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES('repository',$1,$2,0)", &[&pin,&selected.blake3]).unwrap();
    assert!(matches!(
        f.store.mark_ready(&f.credential, &work, &graph),
        Err(alt_server_store::Error::Precondition(
            "object pin expired or fenced"
        ))
    ));
    fn restore_pin(f: &Fixture, pin: &str, digest: &str) {
        f.client().execute("UPDATE alt_pins SET expires_at=(SELECT expires_at FROM alt_requests WHERE request_id='bulk') WHERE repository='repository' AND pin_id=$1 AND blake3=$2", &[&pin,&digest]).unwrap();
    }
    restore_pin(&f, &pin, &selected.blake3);
    f.client()
        .execute(
            "UPDATE alt_blobs SET state='deleting' WHERE repository='repository' AND blake3=$1",
            &[&selected.blake3],
        )
        .unwrap();
    assert!(matches!(
        f.store.mark_ready(&f.credential, &work, &graph),
        Err(alt_server_store::Error::Precondition(
            "object pin expired or fenced"
        ))
    ));
    f.client()
        .execute(
            "UPDATE alt_blobs SET state='stored' WHERE repository='repository' AND blake3=$1",
            &[&selected.blake3],
        )
        .unwrap();
    f.client()
        .execute(
            "DELETE FROM alt_pins WHERE repository='repository' AND pin_id='unrelated-live'",
            &[],
        )
        .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    // Ready is not permission to publish after the exact pin expires.
    f.client().execute("UPDATE alt_pins SET expires_at=0 WHERE repository='repository' AND pin_id=$1 AND blake3=$2", &[&pin,&selected.blake3]).unwrap();
    assert!(matches!(
        f.store.publish(&f.credential, &work, &graph),
        Err(alt_server_store::Error::Precondition(
            "object pin expired or fenced"
        ))
    ));
    assert_eq!(f.counts(), (0, 0, 0));
    restore_pin(&f, &pin, &selected.blake3);
    f.store.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
    let snapshot = f
        .store
        .read_snapshot(&f.credential, "repository", 60_000, 5000)
        .unwrap();
    assert_eq!(snapshot.objects().count(), p.contents.len());
    let (_, _, mut file) = f
        .store
        .open_snapshot_object(&f.credential, &snapshot, oid.parse().unwrap())
        .unwrap();
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut raw).unwrap();
    assert_eq!(raw, p.contents[1099]);
    let credential = f.credential.clone();
    let buffered = f
        .store
        .with_snapshot_objects(
            &credential,
            &snapshot,
            |objects| {
                use alt_repository_service::ObjectSource;
                let mut object = objects.open(oid.parse().unwrap())?.unwrap();
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut object.reader, &mut bytes)?;
                Ok(bytes)
            },
            |_| Ok(credential.clone()),
        )
        .unwrap();
    assert_eq!(buffered, p.contents[1099]);
    // nothing is retained in the content store for objects a segment holds
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_pins", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL; multi-batch registration preserves canonical identities and storage accounting"]
fn bounded_registration_batches_publish_all_objects_without_duplicate_byte_charges() {
    let mut f = Fixture::new();
    f.store = larger_store(&f);
    let p = payload();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let objects: Vec<_> = p.manifest.entries.iter().zip(&p.contents).collect();
    // This fixture creates separate loose files, whose descriptor bound is
    // independent of the larger logical segment registration transaction.
    for batch in objects.chunks(alt_server_store::MAX_PUT_INPUTS) {
        let proofs: Vec<_> = batch
            .iter()
            .map(|(entry, data)| {
                f.store
                    .bytes()
                    .put(
                        &p.publication.key.scope,
                        &entry.blake3,
                        entry.bytes,
                        bulk_limits().object_bytes,
                        &mut Cursor::new(*data),
                    )
                    .unwrap()
            })
            .collect();
        let selected: Vec<_> = batch
            .iter()
            .zip(&proofs)
            .map(|((entry, _), proof)| (*entry, proof))
            .collect();
        f.store
            .register_contents(&f.credential, &p.publication.key, &selected)
            .unwrap();
    }
    let row = f.client().query_one("SELECT (SELECT count(*) FROM alt_objects),(SELECT count(*) FROM alt_blobs WHERE accounted),(SELECT reserved_bytes FROM alt_repositories),(SELECT count(*) FROM alt_request_content WHERE NOT verified)", &[]).unwrap();
    assert_eq!(
        (
            row.get::<_, i64>(0),
            row.get::<_, i64>(1),
            row.get::<_, i64>(2),
            row.get::<_, i64>(3)
        ),
        (1103, 1102, 0, 0)
    );
    let work = f
        .store
        .acquire_publication(&f.credential, &p.publication.key, 30_000, 5000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        GraphLimits {
            objects: 5000,
            paths: 5000,
            ..graph_limits()
        },
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    f.store.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
    for (entry, original) in p.manifest.entries.iter().zip(&p.contents) {
        let mut file = f
            .store
            .bytes()
            .read(&p.publication.key.scope, &entry.blake3, entry.bytes)
            .unwrap();
        let mut actual = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut actual).unwrap();
        assert_eq!(&actual, original);
    }
}
