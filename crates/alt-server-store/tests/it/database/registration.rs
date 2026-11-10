use super::*;
use alt_server_store::{DurableBytes, MAX_REGISTER_CONTENTS};

fn proofs(f: &Fixture, p: &Payload) -> Vec<DurableBytes> {
    let mut sources: Vec<_> = p.contents.iter().map(Cursor::new).collect();
    let mut inputs: Vec<_> = p
        .manifest
        .entries
        .iter()
        .zip(&mut sources)
        .map(|(entry, source)| alt_server_store::ByteInput {
            digest: &entry.blake3,
            bytes: entry.bytes,
            source,
        })
        .collect();
    f.store
        .bytes()
        .put_batch(&p.publication.key.scope, &mut inputs, limits().object_bytes)
        .unwrap()
}

fn catalog(f: &Fixture) -> serde_json::Value {
    f.client().query_one("SELECT jsonb_build_object(
        'repositories',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM alt_repositories r),
        'requests',(SELECT jsonb_agg(to_jsonb(r) ORDER BY request_id) FROM alt_requests r),
        'content',(SELECT jsonb_agg(to_jsonb(r) ORDER BY request_id,blake3) FROM alt_request_content r),
        'blobs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY blake3) FROM alt_blobs r),
        'objects',(SELECT jsonb_agg(to_jsonb(r) ORDER BY oid) FROM alt_objects r),
        'pins',(SELECT jsonb_agg(to_jsonb(r) ORDER BY pin_id,blake3) FROM alt_pins r),
        'ingress',(SELECT jsonb_agg(to_jsonb(r) ORDER BY month) FROM alt_cloud_ingress r),
        'reservations',(SELECT jsonb_agg(to_jsonb(r) ORDER BY request_id) FROM alt_cloud_reservations r))", &[]).unwrap().get(0)
}

pub(super) fn shared_payload() -> Payload {
    let mut p = Payload::new("batch-shared", None);
    for (kind, bytes) in [
        (ContentKind::Blob, b"abcdef".to_vec()),
        (ContentKind::Attachment, b"abcdef".to_vec()),
        (ContentKind::Blob, vec![]),
    ] {
        let digest = blake3::hash(&bytes).to_hex().to_string();
        p.manifest.entries.push(ContentEntry {
            kind,
            blake3: digest.clone(),
            bytes: bytes.len() as u64,
            git_oid: (kind == ContentKind::Blob).then(|| {
                ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &bytes).to_string()
            }),
            chunks: if bytes.is_empty() {
                vec![]
            } else {
                vec![Chunk {
                    offset: 0,
                    bytes: bytes.len() as u64,
                    blake3: digest,
                }]
            },
        });
        p.contents.push(bytes);
    }
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    p
}

#[test]
#[ignore = "requires isolated PostgreSQL; same-byte roles, replay and Cloud accounting within a registration batch"]
fn batched_registration_preserves_shared_roles_and_counts_cloud_roots_once() {
    for cloud in [false, true] {
        let mut f = if cloud {
            let mut f = super::cloud::cloud();
            super::cloud::repository(&mut f);
            f
        } else {
            Fixture::new()
        };
        let p = shared_payload();
        let unique: std::collections::BTreeMap<_, _> = p
            .manifest
            .entries
            .iter()
            .map(|entry| (&entry.blake3, entry.bytes))
            .collect();
        let total = unique.values().sum::<u64>() as i64;
        f.store
            .begin(&f.credential, &p.publication, &p.manifest, 60_000)
            .unwrap();
        let proofs = proofs(&f, &p);
        let mut entries: Vec<_> = p.manifest.entries.iter().zip(&proofs).collect();
        entries.push(entries[0]); // repeated entry/proof is not another charge
        f.store
            .register_contents(&f.credential, &p.publication.key, &entries)
            .unwrap();
        let before = catalog(&f);
        f.store
            .register_contents(&f.credential, &p.publication.key, &entries)
            .unwrap();
        assert_eq!(catalog(&f), before);
        let row = f
            .client()
            .query_one(
                "SELECT stored_bytes,reserved_bytes FROM alt_repositories",
                &[],
            )
            .unwrap();
        assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1)), (total, 0));
        for entry in &p.manifest.entries {
            if let Some(oid) = &entry.git_oid {
                let row = f
                    .client()
                    .query_one("SELECT blake3 FROM alt_objects WHERE oid=$1", &[oid])
                    .unwrap();
                assert_eq!(row.get::<_, String>(0), entry.blake3);
            }
        }
        if cloud {
            let row = f.client().query_one("SELECT sum(accepted_bytes)::bigint,sum(reserved_bytes)::bigint FROM alt_cloud_ingress", &[]).unwrap();
            assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1)), (total, 0));
        }
        p.publish(&mut f.store, &f.credential);
        assert_eq!(f.counts(), (1, 1, 1));
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; late catalog/proof/selection failures cannot partially register a batch"]
fn batch_admission_and_proof_failures_preserve_all_catalog_state() {
    for fault in [
        "selection",
        "gc",
        "mapping",
        "proof",
        "quota",
        "expired",
        "revoked",
        "oversize",
        "empty",
    ] {
        let mut f = Fixture::new();
        let p = Payload::new("batch-fault", None);
        f.store
            .begin(&f.credential, &p.publication, &p.manifest, 60_000)
            .unwrap();
        let mut proofs = proofs(&f, &p);
        let mut selected = p.manifest.entries.clone();
        let last = selected.len() - 1;
        match fault {
            "selection" => {
                selected[last].git_oid = None;
                selected[last].kind = ContentKind::Attachment;
            }
            "gc" => {
                f.client()
                    .execute(
                        "UPDATE alt_blobs SET state='deleting' WHERE blake3=$1",
                        &[&selected[last].blake3],
                    )
                    .unwrap();
            }
            "mapping" => {
                f.client().execute("INSERT INTO alt_objects(repository,oid,kind,blake3) VALUES('repository',$1,'blob',$2)", &[selected[last].git_oid.as_ref().unwrap(), &selected[0].blake3]).unwrap();
            }
            "proof" => {
                let foreign = ByteStore::open(&f.root.path().join("foreign-proof")).unwrap();
                proofs[last] = foreign
                    .put(
                        &p.publication.key.scope,
                        &selected[last].blake3,
                        selected[last].bytes,
                        limits().object_bytes,
                        &mut Cursor::new(&p.contents[last]),
                    )
                    .unwrap();
            }
            "quota" => {
                f.client()
                    .execute("UPDATE alt_repositories SET quota_bytes=0", &[])
                    .unwrap();
            }
            "expired" => {
                f.client()
                    .execute("UPDATE alt_requests SET expires_at=0", &[])
                    .unwrap();
            }
            "revoked" => {
                f.store.revoke(&f.credential, &f.token).unwrap();
            }
            _ => (),
        }
        let mut batch: Vec<_> = selected.iter().zip(&proofs).collect();
        if fault == "oversize" {
            batch.resize(MAX_REGISTER_CONTENTS + 1, batch[0]);
        }
        if fault == "empty" {
            batch.clear();
        }
        let before = catalog(&f);
        assert!(
            f.store
                .register_contents(&f.credential, &p.publication.key, &batch)
                .is_err(),
            "{fault}"
        );
        assert_eq!(catalog(&f), before, "{fault}");
        assert_eq!(f.counts(), (0, 0, 0));
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; reduced Cloud allowance must roll back already-written batch catalog rows"]
fn reduced_cloud_allowance_rolls_back_the_whole_registration_batch() {
    use golia_platform_contract::Counter;
    let mut f = super::cloud::cloud();
    super::cloud::repository(&mut f);
    let p = shared_payload();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let proofs = proofs(&f, &p);
    let mut policy = super::cloud::service(3, 2, true);
    policy.limits.monthly_ingress_bytes = Counter(1);
    f.store.refresh_cloud_service(&policy).unwrap();
    f.store
        .refresh_cloud_account(
            &alt_server_store::cloud_credential_digest(&f.token).unwrap(),
            &super::cloud::claims(4, 2),
        )
        .unwrap();
    f.client().batch_execute("CREATE SEQUENCE test_batch_reached;
        CREATE FUNCTION test_batch_written() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM nextval('test_batch_reached'); RETURN NULL; END $$;
        CREATE TRIGGER test_batch_written AFTER UPDATE ON alt_blobs FOR EACH STATEMENT EXECUTE FUNCTION test_batch_written()").unwrap();
    let before = catalog(&f);
    let batch: Vec<_> = p.manifest.entries.iter().zip(&proofs).collect();
    assert!(matches!(
        f.store
            .register_contents(&f.credential, &p.publication.key, &batch),
        Err(alt_server_store::Error::Limit(
            "Cloud ingress allowance reduced"
        ))
    ));
    assert!(
        f.client()
            .query_one("SELECT is_called FROM test_batch_reached", &[])
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(catalog(&f), before);
}

#[test]
#[ignore = "requires isolated PostgreSQL; real clock expiry after batch SQL writes must prevent commit"]
fn batch_rechecks_request_credentials_and_cloud_leases_after_writes() {
    use golia_platform_contract::Counter;
    for expiry in ["request", "credential", "cloud-service", "cloud-account"] {
        let mut f = if expiry.starts_with("cloud-") {
            let mut f = super::cloud::cloud();
            super::cloud::repository(&mut f);
            f
        } else {
            Fixture::new()
        };
        let p = Payload::new("batch-expiry", None);
        f.store
            .begin(&f.credential, &p.publication, &p.manifest, 60_000)
            .unwrap();
        let proofs = proofs(&f, &p);
        f.client().batch_execute("CREATE SEQUENCE test_batch_reached;
            CREATE FUNCTION test_delay_batch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM nextval('test_batch_reached'); PERFORM pg_sleep(1.1); RETURN NULL; END $$;
            CREATE TRIGGER test_delay_batch AFTER UPDATE ON alt_request_content FOR EACH STATEMENT EXECUTE FUNCTION test_delay_batch()" ).unwrap();
        let deadline: i64 = f
            .client()
            .query_one(
                "SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint+1000",
                &[],
            )
            .unwrap()
            .get(0);
        match expiry {
            "request" => {
                f.client()
                    .execute("UPDATE alt_requests SET expires_at=$1", &[&deadline])
                    .unwrap();
            }
            "credential" => {
                f.client()
                    .execute("UPDATE alt_credentials SET expires_at=$1", &[&deadline])
                    .unwrap();
            }
            "cloud-service" => {
                let mut policy = super::cloud::service(3, 1, true);
                policy.lease.expires_at_unix_ms = Counter(deadline as u64);
                f.store.refresh_cloud_service(&policy).unwrap();
            }
            "cloud-account" => {
                let mut claims = super::cloud::claims(3, 1);
                claims.lease.expires_at_unix_ms = Counter(deadline as u64);
                f.store
                    .refresh_cloud_account(
                        &alt_server_store::cloud_credential_digest(&f.token).unwrap(),
                        &claims,
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let batch: Vec<_> = p.manifest.entries.iter().zip(&proofs).collect();
        let before = catalog(&f);
        assert!(
            f.store
                .register_contents(&f.credential, &p.publication.key, &batch)
                .is_err(),
            "{expiry}"
        );
        assert!(
            f.client()
                .query_one("SELECT is_called FROM test_batch_reached", &[])
                .unwrap()
                .get::<_, bool>(0),
            "{expiry}: rejection must occur after batch writes"
        );
        assert_eq!(catalog(&f), before, "{expiry}");
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; failed byte batch leaves the reserved publication invisible"]
fn failed_byte_batch_does_not_advance_request_or_catalog() {
    let mut f = Fixture::new();
    let p = Payload::new("bad-byte-batch", None);
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let before = catalog(&f);
    let mut data = p.contents.clone();
    let last = data.len() - 1;
    data[last].push(0);
    let mut sources: Vec<_> = data.iter().map(Cursor::new).collect();
    let mut inputs: Vec<_> = p
        .manifest
        .entries
        .iter()
        .zip(&mut sources)
        .map(|(entry, source)| alt_server_store::ByteInput {
            digest: &entry.blake3,
            bytes: entry.bytes,
            source,
        })
        .collect();
    assert!(
        f.store
            .bytes()
            .put_batch(&p.publication.key.scope, &mut inputs, limits().object_bytes)
            .is_err()
    );
    assert_eq!(catalog(&f), before);
    assert_eq!(f.counts(), (0, 0, 0));
    let proofs = proofs(&f, &p);
    let selected: Vec<_> = p.manifest.entries.iter().zip(&proofs).collect();
    f.store
        .register_contents(&f.credential, &p.publication.key, &selected)
        .unwrap();
    p.publish(&mut f.store, &f.credential);
    assert_eq!(f.counts(), (1, 1, 1));
}
