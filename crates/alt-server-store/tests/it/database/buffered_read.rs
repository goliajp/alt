use super::*;
use alt_repository_service::ObjectSource;
use std::io::Read;
use std::sync::{Arc, atomic::AtomicBool};

struct Buffered {
    dropped: Arc<AtomicBool>,
}

#[test]
#[ignore = "requires isolated PostgreSQL; checks paged snapshot selection and exact object limits"]
fn snapshot_selection_pages_past_unpublished_objects_and_counts_mappings() {
    let mut f = Fixture::new();
    // Metadata-only fixture: no byte reader is invoked. More than six pages of
    // unpublished mappings must not hide the published objects that follow.
    f.client().batch_execute(
        "INSERT INTO alt_blobs(repository,blake3,bytes,state,created_at)
         SELECT 'repository',lpad(to_hex(n),64,'0'),0,'stored',0 FROM generate_series(1,6207) n;
         INSERT INTO alt_objects(repository,oid,kind,blake3)
         SELECT 'repository',lpad(to_hex(n),40,'0'),'blob',lpad(to_hex(n),64,'0') FROM generate_series(1,6207) n;
         UPDATE alt_objects SET blake3=lpad(to_hex(6205),64,'0') WHERE oid=lpad(to_hex(6206),40,'0');
         INSERT INTO alt_pins(repository,pin_id,blake3,expires_at)
         SELECT 'repository','published',lpad(to_hex(n),64,'0'),NULL FROM unnest(ARRAY[6201,6204,6205]) n;
         INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES
         ('repository','another-publication',lpad(to_hex(6205),64,'0'),NULL),
         ('repository','upload:live',lpad(to_hex(6202),64,'0'),9223372036854775807),
         ('repository','read:expired',lpad(to_hex(6203),64,'0'),0);
         UPDATE alt_blobs SET state='deleting' WHERE blake3=lpad(to_hex(6204),64,'0');"
    ).unwrap();
    let read_pin_count = |f: &Fixture| -> i64 {
        f.client().query_one(
            "SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'read:%' AND pin_id<>'read:expired'", &[],
        ).unwrap().get(0)
    };
    assert!(matches!(
        f.store
            .read_snapshot(&f.credential, "repository", 60_000, 2),
        Err(alt_server_store::Error::Limit("read graph objects"))
    ));
    assert_eq!(read_pin_count(&f), 0);
    let read = f
        .store
        .read_snapshot(&f.credential, "repository", 60_000, 3)
        .unwrap();
    let actual: Vec<_> = read
        .objects()
        .map(|(oid, kind, bytes)| {
            assert_eq!(kind, ObjectKind::Blob);
            assert_eq!(bytes, 0);
            oid.to_string()
        })
        .collect();
    let expected: Vec<_> = [6201, 6205, 6206]
        .into_iter()
        .map(|n| format!("{n:040x}"))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(
        read_pin_count(&f),
        2,
        "equal-byte mappings share one read pin"
    );
    f.store.release_snapshot(&f.credential, read).unwrap();
    assert_eq!(read_pin_count(&f), 0);
}
impl Drop for Buffered {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; metadata snapshots expose no contents and retain current authority and expiry checks"]
fn metadata_snapshots_do_not_pin_objects_or_bypass_final_authority() {
    for fault in [
        "valid",
        "grant",
        "credential-expiry",
        "recovery",
        "generation",
        "expiry",
        "refresh",
    ] {
        let mut f = Fixture::new();
        let payload = Payload::new("metadata", None);
        payload.upload(&mut f.store, &f.credential);
        payload.publish(&mut f.store, &f.credential);
        let pins = |f: &Fixture| -> serde_json::Value {
            f.client()
                .query_one(
                    "SELECT jsonb_agg(to_jsonb(p) ORDER BY repository,pin_id,blake3) FROM alt_pins p",
                    &[],
                )
                .unwrap()
                .get(0)
        };
        let before = pins(&f);
        let credential = f.credential.clone();
        let read = f
            .store
            .read_metadata_snapshot(
                &credential,
                "repository",
                if fault == "expiry" { 1000 } else { 60_000 },
            )
            .unwrap();
        assert_eq!(read.refs.get("refs/heads/main"), Some(&payload.commit));
        assert_eq!(read.objects().count(), 0);
        assert_eq!(pins(&f), before);
        assert!(matches!(
            f.store
                .open_snapshot_object(&credential, &read, payload.commit),
            Err(alt_server_store::Error::Unauthorized)
        ));
        let mut client = f.client();
        client.batch_execute("SET statement_timeout='2s'").unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let computed = Arc::new(AtomicBool::new(false));
        let result = f.store.with_snapshot_objects(
            &credential,
            &read,
            |objects| {
                assert!(objects.open(payload.commit)?.is_none());
                match fault {
                    "grant" => {
                        client
                            .execute("UPDATE alt_grants SET can_read=false", &[])
                            .unwrap();
                    }
                    "credential-expiry" => {
                        client
                            .execute("UPDATE alt_credentials SET expires_at=0", &[])
                            .unwrap();
                    }
                    "recovery" => {
                        client
                            .execute(
                                "UPDATE alt_repositories SET recovery_epoch=recovery_epoch+1",
                                &[],
                            )
                            .unwrap();
                    }
                    "generation" => {
                        client
                            .execute(
                                "UPDATE alt_instance SET runtime_generation=runtime_generation+1",
                                &[],
                            )
                            .unwrap();
                    }
                    "expiry" => std::thread::sleep(std::time::Duration::from_millis(1100)),
                    "valid" | "refresh" => {}
                    _ => unreachable!(),
                }
                computed.store(true, Ordering::SeqCst);
                Ok(Buffered {
                    dropped: dropped.clone(),
                })
            },
            |_| {
                if fault == "refresh" {
                    Err(alt_server_store::Error::Unauthorized)
                } else {
                    Ok(credential.clone())
                }
            },
        );
        assert!(computed.load(Ordering::SeqCst));
        assert_eq!(result.is_ok(), fault == "valid", "{fault}");
        if fault == "valid" {
            assert!(!dropped.load(Ordering::SeqCst));
            drop(result);
            f.store.release_snapshot(&credential, read).unwrap();
            // The ordinary object snapshot still protects and exposes contents.
            let read = f
                .store
                .read_snapshot(&credential, "repository", 60_000, 1000)
                .unwrap();
            assert_eq!(read.objects().count(), 2);
            f.store.release_snapshot(&credential, read).unwrap();
        }
        assert!(
            dropped.load(Ordering::SeqCst),
            "retained result after {fault}"
        );
        assert_eq!(pins(&f), before);
        assert_eq!(f.counts(), (1, 1, 1));
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; metadata snapshots expose no object capability or content pins"]
fn metadata_snapshots_preserve_refs_without_selecting_content() {
    let mut f = Fixture::new();
    let credential = f.credential.clone();
    for published in [false, true] {
        let payload = Payload::new("metadata-only", None);
        if published {
            payload.upload(&mut f.store, &credential);
            payload.publish(&mut f.store, &credential);
        }
        let before: i64 = f
            .client()
            .query_one("SELECT count(*) FROM alt_pins", &[])
            .unwrap()
            .get(0);
        let read = f
            .store
            .read_metadata_snapshot(&credential, "repository", 60_000)
            .unwrap();
        assert_eq!(read.refs.is_empty(), !published);
        if published {
            assert!(read.refs.values().any(|oid| *oid == payload.commit));
        }
        assert_eq!(read.objects().count(), 0);
        let during: i64 = f
            .client()
            .query_one("SELECT count(*) FROM alt_pins", &[])
            .unwrap()
            .get(0);
        assert_eq!(before, during);
        assert!(matches!(
            f.store
                .open_snapshot_object(&credential, &read, payload.commit),
            Err(alt_server_store::Error::Unauthorized)
        ));
        f.store
            .with_snapshot_objects(
                &credential,
                &read,
                |objects| {
                    assert!(objects.open(payload.commit)?.is_none());
                    Ok(())
                },
                |_| Ok(credential.clone()),
            )
            .unwrap();
        f.store.release_snapshot(&credential, read).unwrap();
        let after: i64 = f
            .client()
            .query_one("SELECT count(*) FROM alt_pins", &[])
            .unwrap()
            .get(0);
        assert_eq!(before, after);
    }
    f.store
        .set_grants(&credential, "repository", "owner", false, false, &[])
        .unwrap();
    assert!(
        f.store
            .read_metadata_snapshot(&credential, "repository", 60_000)
            .is_err()
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL; metadata responses retain current authority, recovery and expiry checks"]
fn metadata_responses_discard_results_after_authority_changes() {
    for fault in [
        "grant",
        "credential",
        "credential-expiry",
        "recovery",
        "generation",
        "refresh",
        "snapshot-expiry",
    ] {
        let mut f = Fixture::new();
        let credential = f.credential.clone();
        let ttl = if fault == "snapshot-expiry" {
            1000
        } else {
            60_000
        };
        let read = f
            .store
            .read_metadata_snapshot(&credential, "repository", ttl)
            .unwrap();
        let mut client = f.client();
        client.batch_execute("SET statement_timeout='2s'").unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let computed = Arc::new(AtomicBool::new(false));
        let result = f.store.with_snapshot_objects(
            &credential,
            &read,
            |_| {
                match fault {
                    "grant" => {
                        client
                            .execute("UPDATE alt_grants SET can_read=false", &[])
                            .unwrap();
                    }
                    "credential" => {
                        client
                            .execute("UPDATE alt_credentials SET revoked=true", &[])
                            .unwrap();
                    }
                    "credential-expiry" => {
                        client
                            .execute("UPDATE alt_credentials SET expires_at=0", &[])
                            .unwrap();
                    }
                    "recovery" => {
                        client
                            .execute(
                                "UPDATE alt_repositories SET recovery_epoch=recovery_epoch+1",
                                &[],
                            )
                            .unwrap();
                    }
                    "generation" => {
                        client
                            .execute(
                                "UPDATE alt_instance SET runtime_generation=runtime_generation+1",
                                &[],
                            )
                            .unwrap();
                    }
                    "snapshot-expiry" => std::thread::sleep(std::time::Duration::from_millis(1100)),
                    "refresh" => {}
                    _ => unreachable!(),
                }
                computed.store(true, Ordering::SeqCst);
                Ok(Buffered {
                    dropped: dropped.clone(),
                })
            },
            |_| {
                if fault == "refresh" {
                    Err(alt_server_store::Error::Unauthorized)
                } else {
                    Ok(credential.clone())
                }
            },
        );
        assert!(result.is_err(), "metadata result escaped after {fault}");
        assert!(computed.load(Ordering::SeqCst));
        assert!(dropped.load(Ordering::SeqCst));
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; checks current authority and pin faults during buffered computation"]
fn buffered_reads_drop_results_after_current_authority_or_pin_changes() {
    for references in [false, true] {
        for fault in [
            "grant",
            "credential",
            "credential-expiry",
            "recovery",
            "generation",
            "object-format",
            "pin-expiry",
            "pin-missing",
            "pin-wrong",
            "deleting",
            "size",
            "refresh",
        ] {
            let mut f = Fixture::new();
            let payload = Payload::new("buffered", None);
            payload.upload(&mut f.store, &f.credential);
            payload.publish(&mut f.store, &f.credential);
            let read = if references {
                f.store
                    .read_ref_snapshot(&f.credential, "repository", 60_000, &[])
            } else {
                f.store
                    .read_snapshot(&f.credential, "repository", 60_000, 1000)
            }
            .unwrap();
            let mut control_client = f.client();
            control_client
                .batch_execute("SET statement_timeout='2s'")
                .unwrap();
            let mut control = Store::open(
                control_client,
                f.store.bytes().clone(),
                f.spec.clone(),
                SigningKey::from_bytes(&f.signing_seed),
                limits(),
            )
            .unwrap();
            let mut client = f.client();
            client.batch_execute("SET statement_timeout='2s'").unwrap();
            let credential = f.credential.clone();
            let dropped = Arc::new(AtomicBool::new(false));
            let computed = Arc::new(AtomicBool::new(false));
            let result = f.store.with_snapshot_objects(
            &credential,
            &read,
            |objects| {
                let mut object = objects.open(payload.commit)?.unwrap();
                let mut bytes = Vec::new();
                object.reader.read_to_end(&mut bytes)?;
                assert_eq!(bytes, payload.contents[1]);
                // This independent connection must not wait for a transaction
                // held across local transformation work.
                match fault {
                    "grant" => control.set_grants(&credential, "repository", "owner", false, false, &[]).unwrap(),
                    "credential" => control.revoke(&credential, &f.token).unwrap(),
                    "credential-expiry" => { client.execute("UPDATE alt_credentials SET expires_at=0", &[]).unwrap(); },
                    "recovery" => { client.execute("UPDATE alt_repositories SET recovery_epoch=recovery_epoch+1", &[]).unwrap(); },
                    "generation" => { client.execute("UPDATE alt_instance SET runtime_generation=runtime_generation+1", &[]).unwrap(); },
                    "object-format" => { client.batch_execute("ALTER TABLE alt_repositories DROP CONSTRAINT alt_repositories_object_format_check; UPDATE alt_repositories SET object_format='sha256'").unwrap(); },
                    "pin-expiry" => { client.execute("UPDATE alt_pins SET expires_at=0 WHERE expires_at IS NOT NULL", &[]).unwrap(); },
                    "pin-missing" => { client.execute("DELETE FROM alt_pins WHERE expires_at IS NOT NULL", &[]).unwrap(); },
                    "pin-wrong" => { client.execute("UPDATE alt_pins SET pin_id='read:other' WHERE expires_at IS NOT NULL", &[]).unwrap(); },
                    "deleting" => { client.execute("UPDATE alt_blobs SET state='deleting'", &[]).unwrap(); },
                    "size" => { client.execute("UPDATE alt_blobs SET bytes=bytes+1", &[]).unwrap(); },
                    "refresh" => {},
                    _ => unreachable!(),
                }
                computed.store(true, Ordering::SeqCst);
                Ok(Buffered { dropped: dropped.clone() })
            },
            |_| {
                if fault == "refresh" { Err(alt_server_store::Error::Unauthorized) }
                else { Ok(credential.clone()) }
            },
        );
            assert!(result.is_err(), "returned a result after {fault}");
            assert!(
                computed.load(Ordering::SeqCst),
                "did not exercise final check: {fault}"
            );
            assert!(
                dropped.load(Ordering::SeqCst),
                "retained failed result: {fault}"
            );
            assert_eq!(f.counts(), (1, 1, 1));
        }
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; checks empty snapshot expiry and failures before computation"]
fn buffered_reads_check_initial_authority_and_empty_snapshot_lifetime() {
    for metadata in [false, true] {
        let mut f = Fixture::new();
        let credential = f.credential.clone();
        let read = if metadata {
            f.store
                .read_metadata_snapshot(&credential, "repository", 60_000)
        } else {
            f.store
                .read_snapshot(&credential, "repository", 60_000, 1000)
        }
        .unwrap();
        f.store
            .set_grants(&credential, "repository", "owner", false, false, &[])
            .unwrap();
        assert!(
            f.store
                .with_snapshot_objects(
                    &credential,
                    &read,
                    |_| -> alt_server_store::Result<()> {
                        panic!("must reject before local computation")
                    },
                    |_| Ok(credential.clone())
                )
                .is_err()
        );
        f.store
            .set_grants(&credential, "repository", "owner", true, true, &grants())
            .unwrap();
        f.store.release_snapshot(&credential, read).unwrap();
        let read = if metadata {
            f.store
                .read_metadata_snapshot(&credential, "repository", 1000)
        } else {
            f.store.read_snapshot(&credential, "repository", 1000, 1000)
        }
        .unwrap();
        assert_eq!(read.objects().count(), 0);
        let result = f.store.with_snapshot_objects(
            &credential,
            &read,
            |_| {
                std::thread::sleep(std::time::Duration::from_millis(1100));
                Ok(())
            },
            |_| Ok(credential.clone()),
        );
        assert!(matches!(
            result,
            Err(alt_server_store::Error::Precondition("read pin expired"))
        ));
        f.store.release_snapshot(&credential, read).unwrap();
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; schema-14 permanent and temporary pins survive indexed migration"]
fn schema_fourteen_upgrade_preserves_original_pins_and_published_reads() {
    let mut f = Fixture::new();
    let payload = Payload::new("before-permanent-index", None);
    payload.upload(&mut f.store, &f.credential);
    let receipt = payload.publish(&mut f.store, &f.credential);
    f.client()
        .batch_execute(
            "INSERT INTO alt_pins(repository,pin_id,blake3,expires_at)
         SELECT repository,'another-holder',blake3,NULL FROM alt_pins
         WHERE expires_at IS NULL GROUP BY repository,blake3;
         INSERT INTO alt_pins(repository,pin_id,blake3,expires_at)
         SELECT repository,'temporary-holder',blake3,9223372036854775807 FROM alt_pins
         WHERE expires_at IS NULL GROUP BY repository,blake3;
         DROP TABLE alt_blob_locations,alt_segments; DROP INDEX alt_pins_permanent_content; DELETE FROM alt_schema WHERE version>=15;",
        )
        .unwrap();
    let pins = |f: &Fixture| -> serde_json::Value {
        f.client()
            .query_one(
                "SELECT jsonb_agg(to_jsonb(p) ORDER BY repository,pin_id,blake3) FROM alt_pins p",
                &[],
            )
            .unwrap()
            .get(0)
    };
    let before = pins(&f);
    assert!(
        Store::open(
            f.client(),
            f.store.bytes().clone(),
            f.spec.clone(),
            SigningKey::from_bytes(&f.signing_seed),
            limits()
        )
        .is_err()
    );
    f.store = Store::migrate_v14(
        f.client(),
        f.store.bytes().clone(),
        f.spec.clone(),
        SigningKey::from_bytes(&f.signing_seed),
        limits(),
    )
    .unwrap();
    f.spec.runtime_generation += 1;
    assert_eq!(pins(&f), before);
    let current = f.store.authenticate(&f.token).unwrap();
    assert_eq!(
        f.store.query(&current, &payload.publication.key).unwrap(),
        RequestObservation::Recorded {
            key: payload.publication.key.clone(),
            result: Box::new(RequestState::Committed {
                receipt: Box::new(receipt)
            })
        }
    );
    let read = f
        .store
        .read_snapshot(&current, "repository", 60_000, 1000)
        .unwrap();
    f.store
        .with_snapshot_objects(
            &current,
            &read,
            |objects| {
                let mut object = objects.open(payload.commit)?.unwrap();
                let mut actual = Vec::new();
                object.reader.read_to_end(&mut actual)?;
                assert_eq!(actual, payload.contents[1]);
                Ok(())
            },
            |_| Ok(current.clone()),
        )
        .unwrap();
    f.store.release_snapshot(&current, read).unwrap();
    assert_eq!(pins(&f), before);
    let index: String = f
        .client()
        .query_one(
            "SELECT indexdef FROM pg_indexes WHERE indexname='alt_pins_permanent_content'",
            &[],
        )
        .unwrap()
        .get(0);
    assert!(
        index.contains("(repository, blake3) WHERE (expires_at IS NULL)"),
        "{index}"
    );
    assert!(!index.contains("UNIQUE"));
}

fn tagged_payload() -> Payload {
    let mut p = Payload::new("reference-projection", None);
    let mut target = p.commit;
    for (name, kind) in [("inner", "commit"), ("outer", "tag")] {
        let data = format!("object {target}\ntype {kind}\ntag {name}\ntagger A <a@example.test> 1 +0000\n\n{name}\n").into_bytes();
        target = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tag, &data);
        let digest = blake3::hash(&data).to_hex().to_string();
        p.manifest.entries.push(ContentEntry {
            kind: ContentKind::Tag,
            blake3: digest.clone(),
            bytes: data.len() as u64,
            git_oid: Some(target.to_string()),
            chunks: vec![Chunk {
                offset: 0,
                bytes: data.len() as u64,
                blake3: digest,
            }],
        });
        p.contents.push(data);
    }
    p.publication.refs.push(RefUpdate {
        name: "refs/tags/outer".into(),
        expected: None,
        new: Some(target.to_string()),
        force: false,
    });
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    p
}

#[test]
#[ignore = "requires isolated PostgreSQL; verifies actual nested tags, projected readers, prefixes and original tag faults"]
fn reference_snapshots_pin_only_matching_tips_and_verified_tag_chains() {
    let mut f = Fixture::new();
    let p = tagged_payload();
    p.upload(&mut f.store, &f.credential);
    p.publish(&mut f.store, &f.credential);
    let credential = f.credential.clone();
    for (prefixes, expected) in [
        (vec!["refs/heads/".into()], 1),
        (vec!["HEAD".into()], 1),
        (vec!["refs/tags/".into()], 3),
        (vec!["no-match".into()], 0),
        (vec![], 3),
    ] {
        let read = f
            .store
            .read_ref_snapshot(&credential, "repository", 60_000, &prefixes)
            .unwrap();
        assert_eq!(read.objects().count(), expected);
        let tree = p.manifest.entries[0]
            .git_oid
            .as_ref()
            .unwrap()
            .parse()
            .unwrap();
        assert!(matches!(
            f.store.open_snapshot_object(&credential, &read, tree),
            Err(alt_server_store::Error::Unauthorized)
        ));
        let pin_count: i64 = f
            .client()
            .query_one(
                "SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'read:%'",
                &[],
            )
            .unwrap()
            .get(0);
        assert_eq!(pin_count as usize, expected);
        f.store
            .with_snapshot_objects(
                &credential,
                &read,
                |objects| {
                    for (id, kind, _) in read.objects() {
                        if kind == ObjectKind::Tag {
                            let parsed = alt_repository_service::verify_object(
                                id,
                                objects.open(id)?.unwrap(),
                                graph_limits(),
                            )?;
                            assert!(matches!(parsed, alt_repository_service::Node::Tag { .. }));
                        }
                    }
                    Ok(())
                },
                |_| Ok(credential.clone()),
            )
            .unwrap();
        f.store.release_snapshot(&credential, read).unwrap();
    }
    let read_pins = |f: &Fixture| -> i64 {
        f.client()
            .query_one(
                "SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'read:%'",
                &[],
            )
            .unwrap()
            .get(0)
    };
    assert_eq!(read_pins(&f), 0);
    let tag = &p.manifest.entries[2];
    let path = f
        .root
        .path()
        .join("bytes")
        .join(&f.spec.installation_id)
        .join("repository")
        .join(&tag.blake3);
    let original = std::fs::read(&path).unwrap();
    let mut changed = original.clone();
    *changed.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &changed).unwrap();
    assert!(
        f.store
            .read_ref_snapshot(&credential, "repository", 60_000, &["refs/tags/".into()])
            .is_err()
    );
    assert_eq!(
        read_pins(&f),
        0,
        "failed expansion must release its prior pins"
    );
    // Excluded tag bytes are not interpreted to list an ordinary branch.
    let read = f
        .store
        .read_ref_snapshot(&credential, "repository", 60_000, &["refs/heads/".into()])
        .unwrap();
    f.store.release_snapshot(&credential, read).unwrap();
    std::fs::write(&path, &original).unwrap();
    for fault in ["kind", "stored", "permanent-pin"] {
        match fault {
            "kind" => {
                f.client()
                    .execute(
                        "UPDATE alt_objects SET kind='blob' WHERE oid=$1",
                        &[&p.commit.to_string()],
                    )
                    .unwrap();
            }
            "stored" => {
                f.client()
                    .execute(
                        "UPDATE alt_blobs SET state='deleting' WHERE blake3=$1",
                        &[&p.manifest.entries[1].blake3],
                    )
                    .unwrap();
            }
            "permanent-pin" => {
                f.client()
                    .execute(
                        "UPDATE alt_pins SET expires_at=9223372036854775807 WHERE blake3=$1",
                        &[&p.manifest.entries[1].blake3],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            f.store
                .read_ref_snapshot(&credential, "repository", 60_000, &["refs/tags/".into()])
                .is_err(),
            "{fault}"
        );
        assert_eq!(read_pins(&f), 0, "{fault}");
        f.client()
            .execute(
                "UPDATE alt_objects SET kind='commit' WHERE oid=$1",
                &[&p.commit.to_string()],
            )
            .unwrap();
        f.client()
            .execute(
                "UPDATE alt_blobs SET state='stored' WHERE blake3=$1",
                &[&p.manifest.entries[1].blake3],
            )
            .unwrap();
        f.client()
            .execute(
                "UPDATE alt_pins SET expires_at=NULL WHERE blake3=$1",
                &[&p.manifest.entries[1].blake3],
            )
            .unwrap();
    }
    let full = f
        .store
        .read_snapshot(&credential, "repository", 60_000, 1000)
        .unwrap();
    assert_eq!(full.objects().count(), 4);
    f.store.release_snapshot(&credential, full).unwrap();
}

#[test]
#[ignore = "requires isolated PostgreSQL; reference projection retains bounded expansion and empty-snapshot expiry"]
fn reference_snapshots_enforce_selection_limits_and_empty_expiry() {
    let mut f = Fixture::new();
    let p = tagged_payload();
    p.upload(&mut f.store, &f.credential);
    p.publish(&mut f.store, &f.credential);
    let credential = f.credential.clone();
    let mut bound = limits();
    bound.graph_objects = 1;
    f.store = Store::open(
        f.client(),
        f.store.bytes().clone(),
        f.spec.clone(),
        SigningKey::from_bytes(&f.signing_seed),
        bound,
    )
    .unwrap();
    let read = f
        .store
        .read_ref_snapshot(&credential, "repository", 60_000, &["refs/heads/".into()])
        .unwrap();
    assert_eq!(read.objects().count(), 1);
    f.store.release_snapshot(&credential, read).unwrap();
    assert!(matches!(
        f.store
            .read_ref_snapshot(&credential, "repository", 60_000, &["refs/tags/".into()]),
        Err(alt_server_store::Error::Limit(_))
    ));
    let pins: i64 = f
        .client()
        .query_one(
            "SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'read:%'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(pins, 0);
    assert!(
        f.store
            .read_snapshot(&credential, "repository", 60_000, 1000)
            .is_err()
    );
    let read = f
        .store
        .read_ref_snapshot(&credential, "repository", 1000, &["no-match".into()])
        .unwrap();
    assert_eq!(read.objects().count(), 0);
    let result = f.store.with_snapshot_objects(
        &credential,
        &read,
        |_| {
            std::thread::sleep(std::time::Duration::from_millis(1100));
            Ok(())
        },
        |_| Ok(credential.clone()),
    );
    assert!(matches!(
        result,
        Err(alt_server_store::Error::Precondition("read pin expired"))
    ));
    f.store.release_snapshot(&credential, read).unwrap();
}

#[test]
#[ignore = "requires isolated PostgreSQL; schema 16 upgrade preserves exact receipts and bytes while setting future heap layout"]
fn schema_sixteen_upgrade_preserves_content_and_enables_update_headroom() {
    let mut f = Fixture::new();
    let p = tagged_payload();
    p.upload(&mut f.store, &f.credential);
    let receipt = p.publish(&mut f.store, &f.credential);
    let options = |f: &Fixture| -> Vec<(String, Option<Vec<String>>)> {
        f.client().query("SELECT relname,reloptions FROM pg_class WHERE relname IN ('alt_blobs','alt_request_content') ORDER BY relname", &[]).unwrap().into_iter().map(|r| (r.get(0), r.get(1))).collect()
    };
    assert!(
        options(&f)
            .iter()
            .all(|(_, value)| value.as_deref() == Some(&["fillfactor=50".to_owned()]))
    );
    f.client().batch_execute("ALTER TABLE alt_blobs RESET (fillfactor); ALTER TABLE alt_request_content RESET (fillfactor); DELETE FROM alt_schema WHERE version=17;").unwrap();
    assert!(options(&f).iter().all(|(_, value)| value.is_none()));
    let before: serde_json::Value = f
        .client()
        .query_one(
            "SELECT jsonb_agg(to_jsonb(s) ORDER BY version) FROM alt_schema s",
            &[],
        )
        .unwrap()
        .get(0);
    assert!(
        Store::open(
            f.client(),
            f.store.bytes().clone(),
            f.spec.clone(),
            SigningKey::from_bytes(&f.signing_seed),
            limits()
        )
        .is_err()
    );
    f.store = Store::migrate_v16(
        f.client(),
        f.store.bytes().clone(),
        f.spec.clone(),
        SigningKey::from_bytes(&f.signing_seed),
        limits(),
    )
    .unwrap();
    f.spec.runtime_generation += 1;
    let current = f.store.authenticate(&f.token).unwrap();
    let after: serde_json::Value = f
        .client()
        .query_one(
            "SELECT jsonb_agg(to_jsonb(s) ORDER BY version) FROM alt_schema s WHERE version<=16",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(before, after);
    assert!(
        options(&f)
            .iter()
            .all(|(_, value)| value.as_deref() == Some(&["fillfactor=50".to_owned()]))
    );
    assert_eq!(
        f.store.query(&current, &p.publication.key).unwrap(),
        RequestObservation::Recorded {
            key: p.publication.key.clone(),
            result: Box::new(RequestState::Committed {
                receipt: Box::new(receipt)
            })
        }
    );
    let read = f
        .store
        .read_snapshot(&current, "repository", 60_000, 1000)
        .unwrap();
    f.store
        .with_snapshot_objects(
            &current,
            &read,
            |objects| {
                for (entry, expected) in p.manifest.entries.iter().zip(&p.contents) {
                    let oid = entry.git_oid.as_ref().unwrap().parse().unwrap();
                    let mut object = objects.open(oid)?.unwrap();
                    let mut actual = Vec::new();
                    object.reader.read_to_end(&mut actual)?;
                    assert_eq!(&actual, expected);
                }
                Ok(())
            },
            |_| Ok(current.clone()),
        )
        .unwrap();
    f.store.release_snapshot(&current, read).unwrap();
    let next = Payload::new("after-headroom-migration", Some(p.commit));
    next.upload(&mut f.store, &current);
    next.publish(&mut f.store, &current);
    let read = f
        .store
        .read_ref_snapshot(&current, "repository", 60_000, &[])
        .unwrap();
    assert_eq!(read.refs.get("refs/heads/main"), Some(&next.commit));
    f.store.release_snapshot(&current, read).unwrap();
}
