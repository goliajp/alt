use super::*;
use alt_repository_service::ObjectSource;
use std::io::Read;
use std::sync::{Arc, atomic::AtomicBool};

struct Buffered {
    dropped: Arc<AtomicBool>,
}

#[test]
#[ignore = "requires isolated PostgreSQL; checks snapshot selection from the pack segments and exact object limits"]
fn snapshot_selection_takes_segment_objects_and_enforces_the_exact_limit() {
    let mut f = Fixture::new();
    let p = tagged_payload();
    p.upload(&mut f.store, &f.credential);
    p.publish(&mut f.store, &f.credential);
    // an upload that was never published maps an object id without publishing it
    f.client().batch_execute(
        "INSERT INTO alt_blobs(repository,blake3,bytes,state,created_at) VALUES('repository',lpad('1',64,'0'),0,'stored',0);
         INSERT INTO alt_objects(repository,oid,kind,blake3) VALUES('repository',lpad('1',40,'0'),'blob',lpad('1',64,'0'));
         INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES('repository','upload:live',lpad('1',64,'0'),9223372036854775807);"
    ).unwrap();
    let pins = |f: &Fixture| -> i64 {
        f.client()
            .query_one("SELECT count(*) FROM alt_pins", &[])
            .unwrap()
            .get(0)
    };
    let before = pins(&f);
    assert!(matches!(
        f.store
            .read_snapshot(&f.credential, "repository", 60_000, 3),
        Err(alt_server_store::Error::Limit("read graph objects"))
    ));
    let read = f
        .store
        .read_snapshot(&f.credential, "repository", 60_000, 4)
        .unwrap();
    let actual: Vec<_> = read
        .objects()
        .map(|(oid, kind, bytes)| (oid.to_string(), kind.to_string(), bytes))
        .collect();
    let mut expected: Vec<_> = p
        .manifest
        .entries
        .iter()
        .map(|entry| {
            (
                entry.git_oid.clone().unwrap(),
                serde_json::to_value(entry.kind)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
                entry.bytes,
            )
        })
        .collect();
    expected.sort();
    assert_eq!(actual, expected);
    assert_eq!(
        pins(&f),
        before,
        "a snapshot retains nothing in the content store"
    );
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
            // The ordinary object snapshot still protects and exposes contents.
            let read = f
                .store
                .read_snapshot(&credential, "repository", 60_000, 1000)
                .unwrap();
            assert_eq!(read.objects().count(), 2);
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
#[ignore = "requires isolated PostgreSQL; checks current authority faults during buffered computation"]
fn buffered_reads_drop_results_after_current_authority_changes() {
    for references in [false, true] {
        for fault in [
            "grant",
            "credential",
            "credential-expiry",
            "recovery",
            "generation",
            "object-format",
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
            Err(alt_server_store::Error::Precondition(
                "read snapshot expired"
            ))
        ));
    }
}

fn tagged_payload() -> Payload {
    let mut p = Payload::new("reference-projection", None);
    let mut target = p.commit;
    for (name, kind) in [("inner", "commit"), ("outer", "tag")] {
        let data = format!("object {target}\ntype {kind}\ntag {name}\ntagger A <a@example.test> 1 +0000\n\n{name}\n").into_bytes();
        target = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tag, &data);
        let digest = alt_hash::blake3::hash(&data).to_hex().to_string();
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
fn reference_snapshots_select_only_matching_tips_and_verified_tag_chains() {
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
    }
    // flip one stored byte of the outer tag inside its segment file
    let outer: ObjectId = p.manifest.entries[3]
        .git_oid
        .as_ref()
        .unwrap()
        .parse()
        .unwrap();
    let view = f.store.pack_view(&credential, "repository").unwrap();
    let record = view
        .segments
        .record(&<[u8; 20]>::try_from(outer.as_bytes()).unwrap())
        .unwrap()
        .unwrap();
    let segment: String = f
        .client()
        .query_one("SELECT segment_id FROM alt_pack_segments", &[])
        .unwrap()
        .get(0);
    let path = f.store.pack_path("repository", &segment).unwrap();
    let flip = |path: &std::path::Path| {
        use std::os::unix::fs::FileExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let at = record.offset + u64::from(record.len) - 1;
        let mut byte = [0u8];
        file.read_exact_at(&mut byte, at).unwrap();
        byte[0] ^= 1;
        file.write_all_at(&byte, at).unwrap();
    };
    flip(&path);
    assert!(
        f.store
            .read_ref_snapshot(&credential, "repository", 60_000, &["refs/tags/".into()])
            .is_err()
    );
    // Excluded tag bytes are not interpreted to list an ordinary branch.
    f.store
        .read_ref_snapshot(&credential, "repository", 60_000, &["refs/heads/".into()])
        .unwrap();
    flip(&path);
    assert_eq!(
        f.store
            .read_ref_snapshot(&credential, "repository", 60_000, &["refs/tags/".into()])
            .unwrap()
            .objects()
            .count(),
        3
    );
    let full = f
        .store
        .read_snapshot(&credential, "repository", 60_000, 1000)
        .unwrap();
    assert_eq!(full.objects().count(), 4);
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
    assert!(matches!(
        f.store
            .read_ref_snapshot(&credential, "repository", 60_000, &["refs/tags/".into()]),
        Err(alt_server_store::Error::Limit(_))
    ));
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
        Err(alt_server_store::Error::Precondition(
            "read snapshot expired"
        ))
    ));
}

#[test]
#[ignore = "requires isolated PostgreSQL; snapshots of objects no content-store row describes"]
fn snapshots_read_objects_held_only_by_a_segment() {
    let mut f = Fixture::new();
    let p = tagged_payload();
    p.upload(&mut f.store, &f.credential);
    p.publish(&mut f.store, &f.credential);
    // the state of objects that arrived by git push: no copy in the content store
    f.client()
        .execute("DELETE FROM alt_pins WHERE pin_id LIKE 'ref:%'", &[])
        .unwrap();
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    let rows: i64 = f
        .client()
        .query_one(
            "SELECT (SELECT count(*) FROM alt_blobs)+(SELECT count(*) FROM alt_objects)",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(rows, 0);
    let credential = f.credential.clone();
    let expected: std::collections::BTreeMap<ObjectId, &Vec<u8>> = p
        .manifest
        .entries
        .iter()
        .zip(&p.contents)
        .map(|(entry, bytes)| (entry.git_oid.as_ref().unwrap().parse().unwrap(), bytes))
        .collect();
    let read = f
        .store
        .read_snapshot(&credential, "repository", 60_000, 1000)
        .unwrap();
    assert_eq!(
        read.objects().map(|(oid, _, _)| oid).collect::<Vec<_>>(),
        expected.keys().copied().collect::<Vec<_>>()
    );
    for (oid, _, size) in read.objects() {
        let (_, opened, mut reader) = f
            .store
            .open_snapshot_object(&credential, &read, oid)
            .unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(
            (size, opened, &bytes),
            (bytes.len() as u64, size, expected[&oid])
        );
    }
    let buffered = f
        .store
        .with_snapshot_objects(
            &credential,
            &read,
            |objects| {
                let mut all = Vec::new();
                for (oid, _, _) in read.objects() {
                    let mut bytes = Vec::new();
                    objects.open(oid)?.unwrap().reader.read_to_end(&mut bytes)?;
                    all.push(bytes);
                }
                Ok(all)
            },
            |_| Ok(credential.clone()),
        )
        .unwrap();
    assert_eq!(
        buffered,
        expected.values().map(|b| (*b).clone()).collect::<Vec<_>>()
    );
    // the tag chain of a ref is followed through the segments as well
    let tips = f
        .store
        .read_ref_snapshot(&credential, "repository", 60_000, &["refs/tags/".into()])
        .unwrap();
    assert_eq!(tips.objects().count(), 3);
    assert_eq!(read.refs, tips.refs);
}
