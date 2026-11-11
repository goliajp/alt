use alt_crypto::ed25519::SigningKey;
use alt_provenance::{encode_hex, sign_export, sign_transfer, verify_origin};
use alt_remote_protocol::*;

fn limits() -> Limits {
    Limits {
        request_bytes: 16384,
        ref_updates: 32,
        record_roots: 32,
        manifest_entries: 32,
        graph_objects: 1000,
        graph_edges: 10000,
        structured_bytes: 1_000_000,
        tree_paths: 1000,
        path_bytes: 4096,
        walk_steps: 100_000,
        chunk_bytes: 1024,
        object_bytes: 1024,
        total_bytes: 4096,
    }
}
fn scope(name: &str, epoch: u64) -> Scope {
    Scope {
        installation_id: format!("installation-{name}"),
        repository_instance_id: format!("repository-{name}"),
        recovery_epoch: epoch,
    }
}
fn key(number: u8) -> SigningKey {
    SigningKey::from_bytes(&[number; 32])
}
fn public(key: &SigningKey) -> String {
    encode_hex(key.verifying_key().as_bytes())
}
fn original() -> (Publication, Receipt) {
    let publication = Publication {
        protocol_version: VERSION,
        key: RequestKey {
            scope: scope("a", 1),
            actor_id: "original-author".into(),
            request_id: "original-request".into(),
        },
        attribution: Attribution {
            kind: ActorKind::Human,
            on_behalf_of: None,
            delegation_id: None,
            agent_session: None,
        },
        object_format: ObjectFormat::Sha1,
        refs: vec![RefUpdate {
            name: "refs/heads/main".into(),
            expected: None,
            new: Some("1".repeat(40)),
            force: false,
        }],
        record_roots: vec![RecordRoot {
            kind: RecordKind::Task,
            id: "2".repeat(64),
        }],
        content_manifest: "3".repeat(64),
    };
    let body = ReceiptBody {
        request_digest: publication.digest(limits()).unwrap(),
        publication: publication.clone(),
        transaction_sequence: 1,
        policy_version: 1,
        committed_at_unix_ms: 42,
    };
    let signer = key(1);
    let receipt = Receipt {
        signature: encode_hex(
            &signer
                .sign(&body.signing_bytes(limits()).unwrap())
                .to_bytes(),
        ),
        signing_key_id: alt_hash::blake3::hash(signer.verifying_key().as_bytes())
            .to_hex()
            .to_string(),
        body,
    };
    (publication, receipt)
}
fn recovery(
    from: Scope,
    to: Scope,
    signer: &SigningKey,
    previous: Option<String>,
) -> TransferProof {
    let (publication, _) = original();
    sign_transfer(
        TransferBody {
            version: 1,
            publication_digest: publication.digest(limits()).unwrap(),
            source: from,
            target: to,
            source_public_key: public(signer),
            target_public_key: public(signer),
            operation_id: "recovery-operation".into(),
            previous_proof: previous,
            reason: TransferReason::Recovery {
                backup_digest: "4".repeat(64),
            },
        },
        signer,
    )
    .unwrap()
}
fn migration(
    from: Scope,
    to: Scope,
    source: &SigningKey,
    target: &SigningKey,
    previous: Option<String>,
) -> TransferProof {
    let (publication, _) = original();
    let digest = publication.digest(limits()).unwrap();
    let export = sign_export(
        ExportCertificateBody {
            version: 1,
            source: from.clone(),
            public_key: public(source),
            publication_digest: digest.clone(),
            catalog_digest: "5".repeat(64),
            export_id: "export-operation".into(),
            snapshot_sequence: 10,
        },
        source,
    )
    .unwrap();
    sign_transfer(
        TransferBody {
            version: 1,
            publication_digest: digest,
            source: from,
            target: to,
            source_public_key: public(source),
            target_public_key: public(target),
            operation_id: "import-operation".into(),
            previous_proof: previous,
            reason: TransferReason::Migration {
                export: Box::new(export),
            },
        },
        target,
    )
    .unwrap()
}
fn verify(proofs: &[TransferProof], target: &Scope, signer: &SigningKey) -> bool {
    let (publication, receipt) = original();
    verify_origin(
        &publication,
        &receipt,
        proofs,
        target,
        signer.verifying_key().as_bytes(),
        limits(),
    )
    .is_ok()
}
fn resign(proof: &mut TransferProof, signer: &SigningKey) {
    proof.signature = encode_hex(&signer.sign(&proof.body.signing_bytes().unwrap()).to_bytes());
}

#[test]
fn original_receipt_requires_exact_current_scope_key_and_intent() {
    assert!(verify(&[], &scope("a", 1), &key(1)));
    assert!(!verify(&[], &scope("a", 2), &key(1)));
    assert!(!verify(&[], &scope("b", 1), &key(1)));
    assert!(!verify(&[], &scope("a", 1), &key(2)));
    let (mut publication, receipt) = original();
    publication.refs[0].new = Some("6".repeat(40));
    assert!(
        verify_origin(
            &publication,
            &receipt,
            &[],
            &scope("a", 1),
            key(1).verifying_key().as_bytes(),
            limits()
        )
        .is_err()
    );
}

#[test]
fn recovery_binds_original_signature_to_explicit_advanced_epoch() {
    let (_, receipt) = original();
    let before = serde_json::to_vec(&receipt).unwrap();
    let proof = recovery(scope("a", 1), scope("a", 10), &key(1), None);
    assert!(verify(
        std::slice::from_ref(&proof),
        &scope("a", 10),
        &key(1)
    ));
    assert!(!verify(
        std::slice::from_ref(&proof),
        &scope("a", 11),
        &key(1)
    ));
    assert_eq!(serde_json::to_vec(&receipt).unwrap(), before);
    let mut changed = proof;
    changed.body.reason = TransferReason::Recovery {
        backup_digest: "7".repeat(64),
    };
    assert!(!verify(&[changed], &scope("a", 10), &key(1)));
}

#[test]
fn multiple_migrations_and_recovery_preserve_the_original_authority_chain() {
    let first = migration(scope("a", 1), scope("b", 1), &key(1), &key(2), None);
    let second = migration(
        scope("b", 1),
        scope("c", 1),
        &key(2),
        &key(3),
        Some(first.digest().unwrap()),
    );
    let third = recovery(
        scope("c", 1),
        scope("c", 10),
        &key(3),
        Some(second.digest().unwrap()),
    );
    let proofs = [first, second, third];
    assert!(verify(&proofs, &scope("c", 10), &key(3)));
    assert!(!verify(&proofs, &scope("c", 10), &key(1)));
    assert!(!verify(&proofs[..2], &scope("c", 10), &key(3)));
    assert!(!verify(&proofs[1..], &scope("c", 10), &key(3)));
}

#[test]
fn valid_target_signature_cannot_bless_a_forged_source_export() {
    let mut proof = migration(scope("a", 1), scope("b", 1), &key(1), &key(2), None);
    let TransferReason::Migration { export } = &mut proof.body.reason else {
        unreachable!()
    };
    export.body.catalog_digest = "8".repeat(64);
    // Bypass the signing helper to model a malicious destination with its own key.
    resign(&mut proof, &key(2));
    assert!(!verify(&[proof.clone()], &scope("b", 1), &key(2)));
    assert!(sign_transfer(proof.body, &key(2)).is_err());
}

#[test]
fn signed_chain_cannot_be_spliced_reordered_or_replayed_at_another_scope() {
    let first = migration(scope("a", 1), scope("b", 1), &key(1), &key(2), None);
    let second = migration(
        scope("b", 1),
        scope("c", 1),
        &key(2),
        &key(3),
        Some(first.digest().unwrap()),
    );
    let mut changed = second.clone();
    changed.body.previous_proof = Some("9".repeat(64));
    resign(&mut changed, &key(3));
    assert!(!verify(&[first.clone(), changed], &scope("c", 1), &key(3)));
    assert!(!verify(
        &[second.clone(), first.clone()],
        &scope("c", 1),
        &key(3)
    ));
    assert!(!verify(
        &[first.clone(), second],
        &scope("another", 1),
        &key(3)
    ));
    let mut unsigned = first;
    unsigned.body.operation_id.push('x');
    assert!(!verify(&[unsigned], &scope("b", 1), &key(2)));
}

#[test]
fn recovery_cannot_rotate_identity_or_key_or_move_epoch_backwards() {
    let proof = recovery(scope("a", 1), scope("a", 10), &key(1), None);
    for mutate in [
        (|body: &mut TransferBody| body.target.installation_id.push('x')) as fn(&mut TransferBody),
        |body| body.target.repository_instance_id.push('x'),
        |body| body.target_public_key = public(&key(2)),
        |body| body.target.recovery_epoch = 1,
        |body| body.source.recovery_epoch = 11,
    ] {
        let mut body = proof.body.clone();
        mutate(&mut body);
        assert!(sign_transfer(body, &key(1)).is_err());
    }
    let mut bad_key = proof.body;
    bad_key.target_public_key = "A".repeat(64);
    assert!(bad_key.validate().is_err());
}

#[test]
fn wire_rejects_extensions_and_future_versions_and_preserves_full_width_counters() {
    let proof = migration(scope("a", 1), scope("b", 1), &key(1), &key(2), None);
    let mut value = serde_json::to_value(&proof).unwrap();
    value["body"]["ignored_authority"] = true.into();
    assert!(serde_json::from_value::<TransferProof>(value).is_err());
    let mut body = proof.body;
    body.version = 2;
    assert!(body.validate().is_err());
    body.version = 1;
    let TransferReason::Migration { export } = &mut body.reason else {
        unreachable!()
    };
    export.body.snapshot_sequence = u64::MAX;
    let value = serde_json::to_value(&export.body).unwrap();
    assert_eq!(value["snapshot_sequence"], u64::MAX.to_string());
    assert_eq!(
        serde_json::from_value::<ExportCertificateBody>(value).unwrap(),
        export.body
    );
}

#[test]
fn chain_budget_is_enforced_before_accepting_more_hops() {
    let mut proofs = Vec::new();
    for n in 0..=MAX_TRANSFER_PROOFS {
        let previous = proofs
            .last()
            .map(|proof: &TransferProof| proof.digest().unwrap());
        proofs.push(recovery(
            scope("a", n as u64 + 1),
            scope("a", n as u64 + 2),
            &key(1),
            previous,
        ));
    }
    assert!(verify(
        &proofs[..MAX_TRANSFER_PROOFS],
        &scope("a", MAX_TRANSFER_PROOFS as u64 + 1),
        &key(1)
    ));
    assert!(!verify(
        &proofs,
        &scope("a", MAX_TRANSFER_PROOFS as u64 + 2),
        &key(1)
    ));
}
