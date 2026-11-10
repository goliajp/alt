use alt_portable::*;
use alt_provenance::encode_hex;
use alt_remote_protocol::*;
use ed25519_dalek::{Signer, SigningKey};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{Cursor, Read, Write},
    rc::Rc,
};

fn limits() -> PackageLimits {
    PackageLimits {
        metadata_bytes: 100_000,
        entries: 100,
        publications: 10,
        blob_bytes: 100_000,
        total_bytes: 1_000_000,
        protocol: Limits {
            request_bytes: 20000,
            ref_updates: 32,
            record_roots: 32,
            manifest_entries: 100,
            graph_objects: 100,
            graph_edges: 1000,
            structured_bytes: 100000,
            tree_paths: 1000,
            path_bytes: 4096,
            walk_steps: 10000,
            chunk_bytes: 100000,
            object_bytes: 100000,
            total_bytes: 1000000,
        },
    }
}
fn key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}
struct Bytes(BTreeMap<String, Vec<u8>>);
impl Bytes {
    fn put(&mut self, bytes: Vec<u8>) -> alt_portable::Blob {
        let blob = alt_portable::Blob::of(&bytes);
        self.0.insert(blob.blake3.clone(), bytes);
        blob
    }
}
impl BlobSource for Bytes {
    fn open(&self, blob: &alt_portable::Blob) -> alt_portable::Result<Box<dyn Read + '_>> {
        Ok(Box::new(Cursor::new(self.0.get(&blob.blake3).unwrap())))
    }
}
fn fixture() -> (Catalog, Bytes) {
    let mut data = Bytes(BTreeMap::new());
    let payload = data.put(b"hello\n".to_vec());
    let oid = "ce013625030ba8dba906f756967f9e9ca394464a".to_string();
    let scope = Scope {
        installation_id: "source".into(),
        repository_instance_id: "repo".into(),
        recovery_epoch: 1,
    };
    let manifest = Manifest {
        protocol_version: 1,
        object_format: ObjectFormat::Sha1,
        entries: vec![ContentEntry {
            kind: ContentKind::Blob,
            blake3: payload.blake3.clone(),
            bytes: payload.bytes,
            git_oid: Some(oid.clone()),
            chunks: vec![Chunk {
                offset: 0,
                bytes: payload.bytes,
                blake3: payload.blake3.clone(),
            }],
        }],
    };
    let publication = Publication {
        protocol_version: 1,
        key: RequestKey {
            scope: scope.clone(),
            actor_id: "author".into(),
            request_id: "original".into(),
        },
        attribution: Attribution {
            kind: ActorKind::Human,
            on_behalf_of: None,
            delegation_id: None,
            agent_session: None,
        },
        object_format: ObjectFormat::Sha1,
        refs: vec![RefUpdate {
            name: "refs/tags/raw".into(),
            expected: None,
            new: Some(oid.clone()),
            force: false,
        }],
        record_roots: vec![],
        content_manifest: manifest.id(limits().protocol).unwrap(),
    };
    let digest = publication.digest(limits().protocol).unwrap();
    let body = ReceiptBody {
        request_digest: digest.clone(),
        publication: publication.clone(),
        transaction_sequence: 1,
        policy_version: 1,
        committed_at_unix_ms: 42,
    };
    let receipt = Receipt {
        signature: encode_hex(
            &key()
                .sign(&body.signing_bytes(limits().protocol).unwrap())
                .to_bytes(),
        ),
        signing_key_id: alt_hash::blake3::hash(key().verifying_key().as_bytes())
            .to_hex()
            .to_string(),
        body,
    };
    let historical = HistoricalPublication {
        digest,
        publication: data.put(serde_json::to_vec_pretty(&publication).unwrap()),
        manifest: data.put(serde_json::to_vec(&manifest).unwrap()),
        receipt: data.put(serde_json::to_vec_pretty(&receipt).unwrap()),
        proofs: vec![],
    };
    let blobs = data
        .0
        .values()
        .map(|v| alt_portable::Blob::of(v))
        .collect::<Vec<_>>();
    (
        Catalog {
            version: 1,
            source: scope,
            source_mode: DeploymentMode::Cloud,
            source_public_key: encode_hex(key().verifying_key().as_bytes()),
            export_id: "snapshot".into(),
            snapshot_sequence: u64::MAX,
            object_format: ObjectFormat::Sha1,
            default_branch: "refs/heads/main".into(),
            protections: vec![],
            refs: vec![Reference {
                name: "refs/tags/raw".into(),
                oid: oid.clone(),
            }],
            objects: vec![Object {
                kind: ContentKind::Blob,
                oid,
                payload,
            }],
            publications: vec![historical],
            import_receipts: vec![],
            change_links: vec![],
            identities: vec![IdentityRequirement {
                installation_id: "source".into(),
                actor_id: "author".into(),
                had_read: true,
                had_publish_records: true,
                had_review: false,
                ref_grants: vec![],
            }],
            blobs,
        },
        data,
    )
}
struct Sink {
    digest: String,
    data: Rc<RefCell<BTreeMap<String, Vec<u8>>>>,
}
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.data
            .borrow_mut()
            .entry(self.digest.clone())
            .or_default()
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn packed() -> (Envelope, Bytes, Vec<u8>) {
    let (catalog, data) = fixture();
    verify_publications(&catalog, &data, limits()).unwrap();
    let envelope = Envelope::sign(catalog, &key(), limits()).unwrap();
    let mut archive = Vec::new();
    write_archive(&mut archive, &envelope, &data, limits()).unwrap();
    (envelope, data, archive)
}
fn unpack(raw: &[u8], envelope: &Envelope) -> alt_portable::Result<Envelope> {
    read_archive(
        &mut Cursor::new(raw),
        &envelope.catalog.source,
        key().verifying_key().as_bytes(),
        limits(),
        |_| Ok(Box::new(std::io::sink())),
    )
}

#[test]
fn signed_stream_round_trip_preserves_original_raw_receipts_and_large_watermark() {
    let (envelope, data, archive) = packed();
    let restored = Rc::new(RefCell::new(BTreeMap::new()));
    let actual = read_archive(
        &mut Cursor::new(&archive),
        &envelope.catalog.source,
        key().verifying_key().as_bytes(),
        limits(),
        |blob| {
            Ok(Box::new(Sink {
                digest: blob.blake3.clone(),
                data: restored.clone(),
            }))
        },
    )
    .unwrap();
    assert_eq!(
        actual.encode(limits()).unwrap(),
        envelope.encode(limits()).unwrap()
    );
    assert_eq!(*restored.borrow(), data.0);
    assert_eq!(actual.catalog.snapshot_sequence, u64::MAX);
    verify_publications(&actual.catalog, &Bytes(restored.borrow().clone()), limits()).unwrap();
}

#[test]
fn untrusted_source_and_metadata_fail_before_staging() {
    let (envelope, _, archive) = packed();
    let calls = std::cell::Cell::new(0);
    let invoke = |scope: &Scope, key: &[u8; 32], raw: &[u8]| {
        read_archive(&mut Cursor::new(raw), scope, key, limits(), |_| {
            calls.set(calls.get() + 1);
            Ok(Box::new(std::io::sink()))
        })
    };
    let wrong = SigningKey::from_bytes(&[8; 32]);
    assert!(
        invoke(
            &envelope.catalog.source,
            wrong.verifying_key().as_bytes(),
            &archive
        )
        .is_err()
    );
    let mut scope = envelope.catalog.source.clone();
    scope.recovery_epoch += 1;
    assert!(invoke(&scope, key().verifying_key().as_bytes(), &archive).is_err());
    let mut changed = archive.clone();
    changed[24] = b'[';
    assert!(
        invoke(
            &envelope.catalog.source,
            key().verifying_key().as_bytes(),
            &changed
        )
        .is_err()
    );
    assert_eq!(calls.get(), 0);
}

#[test]
fn corruption_truncation_missing_completion_and_trailing_bytes_never_succeed() {
    let (envelope, _, archive) = packed();
    let metadata = u64::from_le_bytes(archive[16..24].try_into().unwrap()) as usize;
    for length in [
        0,
        15,
        23,
        24 + metadata,
        archive.len() - 49,
        archive.len() - 1,
    ] {
        assert!(
            unpack(&archive[..length], &envelope).is_err(),
            "length {length}"
        );
    }
    for offset in [24 + metadata, archive.len() - 1, archive.len() - 48] {
        let mut bad = archive.clone();
        bad[offset] ^= 1;
        assert!(unpack(&bad, &envelope).is_err(), "offset {offset}");
    }
    let mut bad = archive;
    bad.push(0);
    assert!(unpack(&bad, &envelope).is_err());
}

#[test]
fn sizes_and_counts_are_bounded_before_allocation_or_staging() {
    let (envelope, _, mut archive) = packed();
    archive[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(unpack(&archive, &envelope).is_err());
    let mut budget = limits();
    budget.total_bytes = budget.blob_bytes;
    let mut catalog = envelope.catalog.clone();
    catalog.blobs[0].bytes = 100000;
    assert!(catalog.validate(budget).is_err());
    budget = limits();
    budget.entries = 1;
    assert!(envelope.catalog.validate(budget).is_err());
}

#[test]
fn catalog_rejects_duplicate_refs_aliases_missing_objects_and_active_secret_extensions() {
    let (catalog, _) = fixture();
    let mut bad = catalog.clone();
    bad.refs.push(bad.refs[0].clone());
    assert!(bad.validate(limits()).is_err());
    let mut bad = catalog.clone();
    bad.objects.clear();
    assert!(bad.validate(limits()).is_err());
    let mut bad = catalog.clone();
    bad.refs[0].name = "refs/heads/../escape".into();
    assert!(bad.validate(limits()).is_err());
    let mut bad = catalog.clone();
    bad.blobs[0].bytes += 1;
    assert!(bad.validate(limits()).is_err());
    let mut bad = catalog.clone();
    bad.version += 1;
    assert!(bad.validate(limits()).is_err());
    let mut value = serde_json::to_value(catalog).unwrap();
    value["credentials"] = serde_json::json!(["token"]);
    assert!(serde_json::from_value::<Catalog>(value).is_err());
}

#[test]
fn complete_signature_cannot_hide_missing_or_replaced_source_certificates() {
    let (envelope, _, _) = packed();
    let mut bad = envelope.clone();
    bad.exports.clear();
    assert!(
        bad.verify(
            &bad.catalog.source,
            key().verifying_key().as_bytes(),
            limits()
        )
        .is_err()
    );
    let mut bad = envelope.clone();
    bad.exports[0].body.snapshot_sequence -= 1;
    assert!(
        bad.verify(
            &bad.catalog.source,
            key().verifying_key().as_bytes(),
            limits()
        )
        .is_err()
    );
    let mut bad = envelope;
    bad.catalog.source_mode = DeploymentMode::SelfHosted;
    assert!(
        bad.verify(
            &bad.catalog.source,
            key().verifying_key().as_bytes(),
            limits()
        )
        .is_err()
    );
}

#[test]
fn historical_publications_require_complete_content_and_qualified_actor_mapping() {
    let (catalog, data) = fixture();
    let mut bad = catalog.clone();
    bad.identities[0].installation_id = "other-source".into();
    assert!(verify_publications(&bad, &data, limits()).is_err());
    let mut bad = catalog.clone();
    bad.blobs
        .retain(|b| b.blake3 != bad.objects[0].payload.blake3);
    bad.objects.clear();
    bad.refs.clear();
    assert!(verify_publications(&bad, &data, limits()).is_err());
    let mut bad = catalog;
    bad.publications[0].digest = "a".repeat(64);
    assert!(verify_publications(&bad, &data, limits()).is_err());
}

#[test]
fn failed_sink_and_changed_source_cannot_produce_a_complete_archive() {
    let (envelope, mut data, archive) = packed();
    assert!(
        read_archive(
            &mut Cursor::new(&archive),
            &envelope.catalog.source,
            key().verifying_key().as_bytes(),
            limits(),
            |_| Err(alt_portable::Error::Invalid("disk full"))
        )
        .is_err()
    );
    data.0.values_mut().next().unwrap().push(0);
    let mut output = Vec::new();
    assert!(write_archive(&mut output, &envelope, &data, limits()).is_err());
    assert!(unpack(&output, &envelope).is_err());
}

fn verification_limits() -> VerificationLimits {
    VerificationLimits {
        package: limits(),
        task: alt_task_service::TaskLimits {
            objects: 100,
            records: 100,
            edges: 1000,
            structured_bytes: 100000,
            total_bytes: 1000000,
            work_bytes: 10000000,
            artifact_bytes: 100000,
            paths: 1000,
            path_bytes: 4096,
            walk_steps: 10000,
        },
        work_bytes: 10000000,
        reads: 1000,
    }
}
fn semantic(
    envelope: &Envelope,
    data: &Bytes,
    budget: VerificationLimits,
) -> alt_portable::Result<VerifiedSnapshot> {
    VerifiedSnapshot::verify(
        envelope,
        &envelope.catalog.source,
        key().verifying_key().as_bytes(),
        data,
        budget,
    )
}
#[test]
fn semantic_validation_rechecks_staged_bytes_and_aggregate_read_budget() {
    let (envelope, mut data, _) = packed();
    let verified = semantic(&envelope, &data, verification_limits()).unwrap();
    assert_eq!(
        verified.catalog_digest(),
        envelope.catalog.digest(limits()).unwrap()
    );
    assert_eq!(verified.graph().objects().count(), 1);
    assert_eq!(verified.publications().len(), 1);
    let mut budget = verification_limits();
    budget.reads = 1;
    assert!(semantic(&envelope, &data, budget).is_err());
    budget = verification_limits();
    budget.work_bytes = 1;
    assert!(semantic(&envelope, &data, budget).is_err());
    data.0.values_mut().next().unwrap()[0] ^= 1;
    assert!(semantic(&envelope, &data, verification_limits()).is_err());
}

#[test]
fn signed_complete_archive_cannot_bless_missing_git_dependencies() {
    let (mut catalog, mut data) = fixture();
    let commit = b"tree 1111111111111111111111111111111111111111\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nmissing tree\n";
    let oid = alt_git_codec::ObjectId::hash_object(
        alt_git_codec::HashAlgo::Sha1,
        alt_git_codec::ObjectKind::Commit,
        commit,
    )
    .to_string();
    let payload = data.put(commit.to_vec());
    catalog.publications.clear();
    catalog.refs = vec![Reference {
        name: "refs/heads/main".into(),
        oid: oid.clone(),
    }];
    catalog.objects = vec![Object {
        kind: ContentKind::Commit,
        oid,
        payload,
    }];
    catalog.blobs = data
        .0
        .values()
        .map(|bytes| alt_portable::Blob::of(bytes))
        .collect();
    let envelope = Envelope::sign(catalog, &key(), limits()).unwrap();
    let mut archive = Vec::new();
    write_archive(&mut archive, &envelope, &data, limits()).unwrap();
    unpack(&archive, &envelope).unwrap();
    let error = semantic(&envelope, &data, verification_limits())
        .err()
        .unwrap();
    assert!(error.to_string().contains("object unavailable"), "{error}");
}

#[test]
fn authentic_publication_and_intact_bytes_do_not_replace_task_closure_validation() {
    let (mut catalog, mut data) = fixture();
    let original = &catalog.publications[0];
    let mut publication: Publication =
        serde_json::from_slice(&data.0[&original.publication.blake3]).unwrap();
    let mut manifest: Manifest =
        serde_json::from_slice(&data.0[&original.manifest.blake3]).unwrap();
    publication.record_roots = vec![RecordRoot {
        kind: RecordKind::Task,
        id: "a".repeat(64),
    }];
    let bundle = alt_task_service::TaskBundle {
        schema_version: 1,
        roots: publication.record_roots.clone(),
        records: vec![],
        companions: vec![],
        sources: vec![],
        recoveries: vec![],
        omissions: vec![],
        attestations: vec![],
    };
    let payload = data.put(serde_json::to_vec(&bundle).unwrap());
    manifest.entries.push(ContentEntry {
        kind: ContentKind::TaskBundle,
        blake3: payload.blake3.clone(),
        bytes: payload.bytes,
        git_oid: None,
        chunks: vec![Chunk {
            offset: 0,
            bytes: payload.bytes,
            blake3: payload.blake3,
        }],
    });
    publication.content_manifest = manifest.id(limits().protocol).unwrap();
    let digest = publication.digest(limits().protocol).unwrap();
    let body = ReceiptBody {
        request_digest: digest.clone(),
        publication: publication.clone(),
        transaction_sequence: 1,
        policy_version: 1,
        committed_at_unix_ms: 42,
    };
    let receipt = Receipt {
        signature: encode_hex(
            &key()
                .sign(&body.signing_bytes(limits().protocol).unwrap())
                .to_bytes(),
        ),
        signing_key_id: alt_hash::blake3::hash(key().verifying_key().as_bytes())
            .to_hex()
            .to_string(),
        body,
    };
    catalog.publications = vec![HistoricalPublication {
        digest,
        publication: data.put(serde_json::to_vec(&publication).unwrap()),
        manifest: data.put(serde_json::to_vec(&manifest).unwrap()),
        receipt: data.put(serde_json::to_vec(&receipt).unwrap()),
        proofs: vec![],
    }];
    catalog.blobs = data
        .0
        .values()
        .map(|bytes| alt_portable::Blob::of(bytes))
        .collect();
    verify_publications(&catalog, &data, limits()).unwrap();
    let envelope = Envelope::sign(catalog, &key(), limits()).unwrap();
    assert!(semantic(&envelope, &data, verification_limits()).is_err());
}

#[test]
fn source_signed_manifest_with_false_chunk_claims_is_rejected() {
    let (mut catalog, mut data) = fixture();
    let original = &catalog.publications[0];
    let mut publication: Publication =
        serde_json::from_slice(&data.0[&original.publication.blake3]).unwrap();
    let mut manifest: Manifest =
        serde_json::from_slice(&data.0[&original.manifest.blake3]).unwrap();
    manifest.entries[0].chunks[0].blake3 = "f".repeat(64);
    publication.content_manifest = manifest.id(limits().protocol).unwrap();
    let digest = publication.digest(limits().protocol).unwrap();
    let body = ReceiptBody {
        request_digest: digest.clone(),
        publication: publication.clone(),
        transaction_sequence: 1,
        policy_version: 1,
        committed_at_unix_ms: 42,
    };
    let receipt = Receipt {
        signature: encode_hex(
            &key()
                .sign(&body.signing_bytes(limits().protocol).unwrap())
                .to_bytes(),
        ),
        signing_key_id: alt_hash::blake3::hash(key().verifying_key().as_bytes())
            .to_hex()
            .to_string(),
        body,
    };
    catalog.publications = vec![HistoricalPublication {
        digest,
        publication: data.put(serde_json::to_vec(&publication).unwrap()),
        manifest: data.put(serde_json::to_vec(&manifest).unwrap()),
        receipt: data.put(serde_json::to_vec(&receipt).unwrap()),
        proofs: vec![],
    }];
    catalog.blobs = data
        .0
        .values()
        .map(|bytes| alt_portable::Blob::of(bytes))
        .collect();
    let envelope = Envelope::sign(catalog, &key(), limits()).unwrap();
    let mut archive = Vec::new();
    write_archive(&mut archive, &envelope, &data, limits()).unwrap();
    unpack(&archive, &envelope).unwrap();
    assert!(matches!(
        semantic(&envelope, &data, verification_limits()),
        Err(alt_portable::Error::Invalid("publication chunk digest"))
    ));
}

#[test]
fn protection_and_path_grants_are_bound_as_history_and_validated() {
    let (mut catalog, _) = fixture();
    catalog.protections.push(alt_repository_service::RefPolicy {
        refs: authorization::RefSelector::Namespace("refs/heads/".into()),
        allow_force: false,
        allow_delete: false,
        require_review: true,
    });
    catalog.identities[0].ref_grants.push(authorization::Grant {
        refs: authorization::RefSelector::Exact("refs/heads/main".into()),
        create: false,
        update: true,
        delete: false,
        force: false,
        paths: Some(vec![b"src".to_vec()]),
    });
    let envelope = Envelope::sign(catalog.clone(), &key(), limits()).unwrap();
    envelope
        .verify(&catalog.source, key().verifying_key().as_bytes(), limits())
        .unwrap();
    let mut altered = envelope.clone();
    altered.catalog.protections[0].allow_force = true;
    assert!(
        altered
            .verify(&catalog.source, key().verifying_key().as_bytes(), limits())
            .is_err()
    );
    catalog.identities[0].ref_grants[0].paths = Some(vec![b"../escape".to_vec()]);
    assert!(catalog.validate(limits()).is_err());
    let mut bad = envelope.catalog;
    bad.default_branch = "refs/tags/main".into();
    assert!(bad.validate(limits()).is_err());
}

fn import_intent(envelope: &Envelope) -> ImportIntent {
    ImportIntent {
        version: 1,
        key: RequestKey {
            scope: Scope {
                installation_id: "target".into(),
                repository_instance_id: "new-repo".into(),
                recovery_epoch: 1,
            },
            actor_id: "importer".into(),
            request_id: "import-1".into(),
        },
        target_public_key: encode_hex(SigningKey::from_bytes(&[8; 32]).verifying_key().as_bytes()),
        source: envelope.catalog.source.clone(),
        source_public_key: envelope.catalog.source_public_key.clone(),
        catalog_digest: envelope.catalog.digest(limits()).unwrap(),
        mappings: envelope
            .catalog
            .identities
            .iter()
            .map(|i| IdentityMapping {
                installation_id: i.installation_id.clone(),
                actor_id: i.actor_id.clone(),
                target_actor: None,
            })
            .collect(),
    }
}
#[test]
fn import_intent_requires_exact_qualified_dispositions_and_pinned_source() {
    let (catalog, _) = fixture();
    let envelope = Envelope::sign(catalog, &key(), limits()).unwrap();
    let intent = import_intent(&envelope);
    intent.validate_envelope(&envelope, limits()).unwrap();
    let mut missing = intent.clone();
    missing.mappings.clear();
    assert!(missing.validate_envelope(&envelope, limits()).is_err());
    let mut duplicate = intent.clone();
    duplicate.mappings.push(duplicate.mappings[0].clone());
    assert!(duplicate.digest(limits()).is_err());
    let mut wrong_origin = intent.clone();
    wrong_origin.mappings[0].installation_id = "another-source".into();
    assert!(wrong_origin.validate_envelope(&envelope, limits()).is_err());
    let mut wrong_key = intent.clone();
    wrong_key.source_public_key =
        encode_hex(SigningKey::from_bytes(&[9; 32]).verifying_key().as_bytes());
    assert!(wrong_key.validate_envelope(&envelope, limits()).is_err());
    let mut same_identity = intent.clone();
    same_identity.key.scope.installation_id = intent.source.installation_id.clone();
    assert!(same_identity.digest(limits()).is_err());
    assert!(
        same_identity
            .validate_envelope(&envelope, limits())
            .is_err()
    );
    let mut same_key = intent.clone();
    same_key.target_public_key = intent.source_public_key.clone();
    assert!(same_key.digest(limits()).is_err());
    assert!(same_key.validate_envelope(&envelope, limits()).is_err());
    let mut unsupported = intent.clone();
    unsupported.version = 2;
    assert!(unsupported.validate_envelope(&envelope, limits()).is_err());
    let mut tiny = limits();
    tiny.metadata_bytes = 1;
    assert!(intent.digest(tiny).is_err());
    let mut changed = intent.clone();
    changed.mappings[0].target_actor = Some("current-user".into());
    changed.validate_envelope(&envelope, limits()).unwrap();
    assert_ne!(
        changed.digest(limits()).unwrap(),
        intent.digest(limits()).unwrap()
    );
    changed.key.actor_id = "another-importer".into();
    assert_ne!(
        changed.digest(limits()).unwrap(),
        intent.digest(limits()).unwrap()
    );
}
#[test]
fn import_acceptance_signs_full_intent_and_exact_decimal_sequence() {
    let (catalog, _) = fixture();
    let envelope = Envelope::sign(catalog, &key(), limits()).unwrap();
    let intent = import_intent(&envelope);
    let target = SigningKey::from_bytes(&[8; 32]);
    let receipt = ImportReceipt::sign(
        ImportReceiptBody {
            version: 1,
            intent: intent.clone(),
            transaction_sequence: u64::MAX,
            committed_at_unix_ms: u64::MAX,
        },
        &target,
        limits(),
    )
    .unwrap();
    receipt
        .verify(&intent, target.verifying_key().as_bytes(), limits())
        .unwrap();
    let encoded = serde_json::to_value(&receipt).unwrap();
    assert_eq!(
        encoded["body"]["transaction_sequence"],
        u64::MAX.to_string()
    );
    assert_eq!(
        serde_json::from_value::<ImportReceipt>(encoded.clone()).unwrap(),
        receipt
    );
    let mut numeric = encoded.clone();
    numeric["body"]["transaction_sequence"] = serde_json::json!(1);
    assert!(serde_json::from_value::<ImportReceipt>(numeric).is_err());
    let mut unknown = encoded;
    unknown["body"]["intent"]["credentials"] = serde_json::json!([]);
    assert!(serde_json::from_value::<ImportReceipt>(unknown).is_err());
    let mut forged = receipt.clone();
    forged.body.intent.mappings[0].target_actor = Some("different-user".into());
    assert!(
        forged
            .verify(
                &forged.body.intent,
                target.verifying_key().as_bytes(),
                limits()
            )
            .is_err()
    );
    let mut different = intent.clone();
    different.key.request_id = "different-import".into();
    assert!(
        receipt
            .verify(&different, target.verifying_key().as_bytes(), limits())
            .is_err()
    );
    assert!(
        receipt
            .verify(&intent, key().verifying_key().as_bytes(), limits())
            .is_err()
    );
    assert!(ImportReceipt::sign(receipt.body, &key(), limits()).is_err());
}

fn migrated(
    mut catalog: Catalog,
    data: &mut Bytes,
    source_key: &SigningKey,
    target_key: &SigningKey,
    installation: &str,
) -> Catalog {
    let envelope = Envelope::sign(catalog.clone(), source_key, limits()).unwrap();
    let mut intent = import_intent(&envelope);
    intent.key.scope.installation_id = installation.into();
    intent.key.scope.repository_instance_id = format!("{installation}-destination");
    intent.target_public_key = encode_hex(target_key.verifying_key().as_bytes());
    intent.mappings[0].target_actor = Some("importer".into());
    intent.validate_envelope(&envelope, limits()).unwrap();
    let operation_id = intent.digest(limits()).unwrap();
    for (publication, export) in catalog.publications.iter_mut().zip(envelope.exports) {
        publication.proofs.push(
            alt_provenance::sign_transfer(
                TransferBody {
                    version: 1,
                    publication_digest: publication.digest.clone(),
                    source: catalog.source.clone(),
                    target: intent.key.scope.clone(),
                    source_public_key: catalog.source_public_key.clone(),
                    target_public_key: intent.target_public_key.clone(),
                    operation_id: operation_id.clone(),
                    previous_proof: publication
                        .proofs
                        .last()
                        .map(TransferProof::digest)
                        .transpose()
                        .unwrap(),
                    reason: TransferReason::Migration {
                        export: Box::new(export),
                    },
                },
                target_key,
            )
            .unwrap(),
        );
    }
    let receipt = ImportReceipt::sign(
        ImportReceiptBody {
            version: 1,
            intent: intent.clone(),
            transaction_sequence: 1,
            committed_at_unix_ms: 100,
        },
        target_key,
        limits(),
    )
    .unwrap();
    // Preserve the actual original bytes, including whitespace.
    catalog
        .import_receipts
        .push(data.put(serde_json::to_vec_pretty(&receipt).unwrap()));
    catalog
        .import_receipts
        .sort_by(|a, b| a.blake3.cmp(&b.blake3));
    catalog.identities.push(IdentityRequirement {
        installation_id: installation.into(),
        actor_id: "importer".into(),
        had_read: false,
        had_publish_records: false,
        had_review: false,
        ref_grants: vec![],
    });
    catalog
        .identities
        .sort_by(|a, b| (&a.installation_id, &a.actor_id).cmp(&(&b.installation_id, &b.actor_id)));
    catalog.source = intent.key.scope;
    catalog.source_public_key = intent.target_public_key;
    catalog.export_id = format!("{installation}-export");
    catalog.snapshot_sequence = 1;
    catalog.blobs = data.0.values().map(|v| alt_portable::Blob::of(v)).collect();
    catalog
}
fn check_migrated(
    catalog: Catalog,
    data: &Bytes,
    signer: &SigningKey,
) -> alt_portable::Result<VerifiedSnapshot> {
    let envelope = Envelope::sign(catalog, signer, limits())?;
    VerifiedSnapshot::verify(
        &envelope,
        &envelope.catalog.source,
        signer.verifying_key().as_bytes(),
        data,
        verification_limits(),
    )
}

#[test]
fn two_migrations_preserve_original_identity_dispositions_and_receipt_bytes() {
    let (catalog, mut data) = fixture();
    let middle = SigningKey::from_bytes(&[8; 32]);
    let catalog = migrated(catalog, &mut data, &key(), &middle, "middle");
    let original = data.0[&catalog.import_receipts[0].blake3].clone();
    check_migrated(catalog.clone(), &data, &middle).unwrap();
    // Returning to a different repository in the original installation is valid.
    let catalog = migrated(catalog, &mut data, &middle, &key(), "source");
    check_migrated(catalog.clone(), &data, &key()).unwrap();
    let envelope = Envelope::sign(catalog, &key(), limits()).unwrap();
    let mut archive = Vec::new();
    write_archive(&mut archive, &envelope, &data, limits()).unwrap();
    unpack(&archive, &envelope).unwrap();
    assert!(
        envelope
            .catalog
            .import_receipts
            .iter()
            .any(|b| data.0[&b.blake3] == original)
    );
    let receipt: ImportReceipt = serde_json::from_slice(&original).unwrap();
    assert_eq!(
        receipt.body.intent.mappings[0].target_actor.as_deref(),
        Some("importer")
    );
    assert_eq!(envelope.catalog.publications[0].proofs.len(), 2);
}

#[test]
fn migration_cannot_drop_or_rewrite_mapping_preimages_even_with_valid_source_signature() {
    let (catalog, mut data) = fixture();
    let target = SigningKey::from_bytes(&[8; 32]);
    let catalog = migrated(catalog, &mut data, &key(), &target, "target");
    let mut missing = catalog.clone();
    missing.import_receipts.clear();
    assert!(matches!(
        check_migrated(missing, &data, &target),
        Err(alt_portable::Error::Invalid(
            "migration import receipt absent"
        ))
    ));
    let mut absent_identity = catalog.clone();
    absent_identity
        .identities
        .retain(|i| i.actor_id != "importer");
    assert!(matches!(
        check_migrated(absent_identity, &data, &target),
        Err(alt_portable::Error::Invalid(
            "historical import identity absent"
        ))
    ));
    let mut changed = catalog.clone();
    let mut receipt: ImportReceipt =
        serde_json::from_slice(&data.0[&catalog.import_receipts[0].blake3]).unwrap();
    receipt.body.intent.mappings[0].target_actor = None;
    // Even a newly valid target signature cannot replace the historical intent.
    let receipt = ImportReceipt::sign(receipt.body, &target, limits()).unwrap();
    changed.import_receipts = vec![data.put(serde_json::to_vec(&receipt).unwrap())];
    changed.blobs = data.0.values().map(|v| alt_portable::Blob::of(v)).collect();
    assert!(matches!(
        check_migrated(changed.clone(), &data, &target),
        Err(alt_portable::Error::Invalid(
            "migration import receipt absent"
        ))
    ));
    // Changing the hop's operation ID too must still bind the source catalog.
    let mut wrong = receipt.body.clone();
    wrong.intent.catalog_digest = "f".repeat(64);
    let wrong = ImportReceipt::sign(wrong, &target, limits()).unwrap();
    changed.import_receipts = vec![data.put(serde_json::to_vec(&wrong).unwrap())];
    changed.blobs = data.0.values().map(|v| alt_portable::Blob::of(v)).collect();
    let mut hop = changed.publications[0].proofs[0].body.clone();
    hop.operation_id = wrong.body.intent.digest(limits()).unwrap();
    changed.publications[0].proofs[0] = alt_provenance::sign_transfer(hop, &target).unwrap();
    assert!(matches!(
        check_migrated(changed, &data, &target),
        Err(alt_portable::Error::Invalid(
            "migration import receipt binding"
        ))
    ));
}

#[test]
fn refs_only_import_receipts_remain_signed_history_without_current_grants() {
    let (mut catalog, mut data) = fixture();
    catalog.publications.clear();
    let target = SigningKey::from_bytes(&[8; 32]);
    let mut catalog = migrated(catalog, &mut data, &key(), &target, "target");
    check_migrated(catalog.clone(), &data, &target).unwrap();
    let receipt_blob = catalog.import_receipts[0].clone();
    let mut receipt: ImportReceipt = serde_json::from_slice(&data.0[&receipt_blob.blake3]).unwrap();
    receipt.signature.replace_range(..2, "00");
    catalog.import_receipts = vec![data.put(serde_json::to_vec(&receipt).unwrap())];
    catalog.blobs = data.0.values().map(|v| alt_portable::Blob::of(v)).collect();
    assert!(check_migrated(catalog, &data, &target).is_err());
}

#[test]
fn archive_inspection_binds_external_source_trust_and_exact_canonical_header() {
    let (catalog, _) = fixture();
    let envelope = Envelope::sign(catalog, &key(), limits()).unwrap();
    let mut request = alt_portable::http::InspectArchive {
        source: envelope.catalog.source.clone(),
        source_public_key: encode_hex(key().verifying_key().as_bytes()),
        metadata: String::from_utf8(envelope.encode(limits()).unwrap()).unwrap(),
    };
    let inspected = request.verify(limits()).unwrap();
    assert_eq!(
        inspected.catalog_digest,
        envelope.catalog.digest(limits()).unwrap()
    );
    assert_eq!(
        inspected.envelope.encode(limits()).unwrap(),
        envelope.encode(limits()).unwrap()
    );
    request.source.recovery_epoch += 1;
    assert!(request.verify(limits()).is_err());
    request.source = envelope.catalog.source.clone();
    request.source_public_key =
        encode_hex(SigningKey::from_bytes(&[8; 32]).verifying_key().as_bytes());
    assert!(request.verify(limits()).is_err());
    request.source_public_key = encode_hex(key().verifying_key().as_bytes());
    request.metadata = serde_json::to_string_pretty(&envelope).unwrap();
    assert!(request.verify(limits()).is_err());
    request.metadata = format!(
        "{{\"catalog\":{},{}",
        serde_json::to_string(&envelope.catalog).unwrap(),
        &String::from_utf8(envelope.encode(limits()).unwrap()).unwrap()[1..]
    );
    assert!(request.verify(limits()).is_err());
    request.metadata = String::from_utf8(envelope.encode(limits()).unwrap()).unwrap();
    let mut tiny = limits();
    tiny.metadata_bytes = request.metadata.len() - 1;
    assert!(matches!(
        request.verify(tiny),
        Err(alt_portable::Error::Limit("archive metadata bytes"))
    ));
}

mod change_links;
