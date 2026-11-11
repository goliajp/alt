use alt_remote_protocol::*;
mod json;
mod login;

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
fn publication() -> Publication {
    Publication {
        protocol_version: VERSION,
        key: RequestKey {
            scope: Scope {
                installation_id: "instance-a".into(),
                repository_instance_id: "repo-a".into(),
                recovery_epoch: 1,
            },
            actor_id: "account-a".into(),
            request_id: "request-a".into(),
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
    }
}
#[test]
fn unordered_intent_has_one_identity_but_every_parameter_is_bound() {
    let mut p = publication();
    p.refs.push(RefUpdate {
        name: "refs/heads/other".into(),
        expected: None,
        new: Some("4".repeat(40)),
        force: false,
    });
    p.record_roots.push(RecordRoot {
        kind: RecordKind::Candidate,
        id: "5".repeat(64),
    });
    let digest = p.digest(limits()).unwrap();
    let mut reordered = p.clone();
    reordered.refs.reverse();
    reordered.record_roots.reverse();
    assert_eq!(reordered.digest(limits()).unwrap(), digest);
    let mutations: Vec<fn(&mut Publication)> = vec![
        |p| p.key.scope.installation_id.push('x'),
        |p| p.key.scope.repository_instance_id.push('x'),
        |p| p.key.scope.recovery_epoch += 1,
        |p| p.key.actor_id.push('x'),
        |p| p.key.request_id.push('x'),
        |p| p.refs[0].name.push('x'),
        |p| p.refs[0].expected = Some("6".repeat(40)),
        |p| p.refs[0].new = Some("7".repeat(40)),
        |p| p.refs[0].force = true,
        |p| p.record_roots[0].kind = RecordKind::Evidence,
        |p| p.record_roots[0].id = "8".repeat(64),
        |p| p.content_manifest = "9".repeat(64),
    ];
    for mutate in mutations {
        let mut changed = p.clone();
        mutate(&mut changed);
        assert_ne!(changed.digest(limits()).unwrap(), digest);
    }
}
#[test]
fn rejects_duplicate_refs_roots_and_ignored_extensions() {
    let p = publication();
    let mut duplicate = p.clone();
    duplicate.refs.push(duplicate.refs[0].clone());
    assert!(duplicate.digest(limits()).is_err());
    let mut duplicate = p.clone();
    duplicate
        .record_roots
        .push(duplicate.record_roots[0].clone());
    assert!(duplicate.digest(limits()).is_err());
    let mut json = serde_json::to_value(&p).unwrap();
    json["ignored_option"] = true.into();
    assert!(Publication::decode(&serde_json::to_vec(&json).unwrap(), limits()).is_err());
    let mut future = p;
    future.protocol_version = 2;
    assert_eq!(future.digest(limits()), Err(Error::Version(2)));
}
#[test]
fn strict_ids_avoid_zero_sentinels_and_hash_algorithm_confusion() {
    for oid in [
        "0".repeat(40),
        "A".repeat(40),
        "1".repeat(64),
        "1".repeat(39),
    ] {
        let mut p = publication();
        p.refs[0].new = Some(oid);
        assert!(p.digest(limits()).is_err());
    }
    let mut p = publication();
    p.object_format = ObjectFormat::Sha256;
    p.refs[0].new = Some("1".repeat(64));
    assert!(p.digest(limits()).is_ok());
    p.key.scope.recovery_epoch = 0;
    assert!(p.digest(limits()).is_err());
}
#[test]
fn invalid_ref_paths_never_reach_a_storage_adapter() {
    for name in [
        "HEAD",
        "refs//main",
        "refs/.hidden/main",
        "refs/a.lock/main",
        "refs/heads/main.",
        "refs/heads/a..b",
        "refs/heads/a@{b",
        "refs/heads/a b",
        "refs/heads/a\\b",
        "refs/heads/a?b",
        "refs/heads/a*b",
        "refs/heads/a[b",
        "refs/heads/a:b",
        "refs/heads/a\nb",
    ] {
        assert!(validate_ref_name(name).is_err(), "{name}");
    }
    for name in [
        "refs/heads/main",
        "refs/notes/alt/meta",
        "refs/tags/v0.1",
        "refs/heads/日本語",
    ] {
        validate_ref_name(name).unwrap();
    }
}
#[test]
fn request_limits_apply_before_json_parse_and_canonical_output() {
    let p = publication();
    let bytes = serde_json::to_vec(&p).unwrap();
    let mut l = limits();
    l.request_bytes = bytes.len() - 1;
    assert_eq!(
        Publication::decode(&bytes, l),
        Err(Error::Limit("request bytes"))
    );
    assert_eq!(p.canonical_bytes(l), Err(Error::Limit("request bytes")));
    let mut p = p;
    p.refs.push(RefUpdate {
        name: "refs/heads/other".into(),
        expected: None,
        new: Some("2".repeat(40)),
        force: false,
    });
    let mut l = limits();
    l.ref_updates = 1;
    assert_eq!(p.digest(l), Err(Error::Limit("reference updates")));
}
fn entry(digest: char) -> ContentEntry {
    ContentEntry {
        kind: ContentKind::Blob,
        blake3: digest.to_string().repeat(64),
        bytes: 512,
        git_oid: Some(digest.to_string().repeat(40)),
        chunks: vec![Chunk {
            offset: 0,
            bytes: 512,
            blake3: digest.to_string().repeat(64),
        }],
    }
}
#[test]
fn manifest_is_order_independent_but_binds_types_sizes_and_mapping() {
    let mut m = Manifest::empty(ObjectFormat::Sha1);
    m.entries = vec![entry('1'), entry('2')];
    let id = m.id(limits()).unwrap();
    m.entries.reverse();
    assert_eq!(m.id(limits()).unwrap(), id);
    m.entries[0].bytes += 1;
    m.entries[0].chunks[0].bytes += 1;
    assert_ne!(m.id(limits()).unwrap(), id);
    m.entries[0].bytes -= 1;
    m.entries[0].chunks[0].bytes -= 1;
    m.entries[0].kind = ContentKind::Tree;
    assert_ne!(m.id(limits()).unwrap(), id);
    m.entries[0].git_oid = Some("3".repeat(40));
    assert_ne!(m.id(limits()).unwrap(), id);
}
#[test]
fn manifest_rejects_aliases_wrong_kinds_and_overflow() {
    let mut m = Manifest::empty(ObjectFormat::Sha1);
    m.entries = vec![entry('1'), entry('1')];
    assert!(m.id(limits()).is_err());
    m.entries = vec![entry('1')];
    m.entries[0].kind = ContentKind::Attachment;
    assert!(m.id(limits()).is_err());
    m.entries[0].git_oid = None;
    assert!(m.id(limits()).is_ok());
    m.entries = vec![entry('1'), entry('2')];
    m.entries[1].git_oid = m.entries[0].git_oid.clone();
    assert!(m.id(limits()).is_err());
    m.entries = vec![entry('1'), entry('2')];
    m.entries[0].bytes = u64::MAX;
    m.entries[0].chunks[0].bytes = u64::MAX;
    m.entries[1].bytes = 1;
    m.entries[1].chunks[0].bytes = 1;
    let mut l = limits();
    l.chunk_bytes = u64::MAX;
    l.object_bytes = u64::MAX;
    l.total_bytes = u64::MAX;
    assert_eq!(m.id(l), Err(Error::Limit("total bytes")));
}
#[test]
fn receipt_authenticates_the_exact_publication_and_transaction() {
    let p = publication();
    let digest = p.digest(limits()).unwrap();
    let mut body = ReceiptBody {
        publication: p,
        request_digest: digest,
        transaction_sequence: 1,
        policy_version: 1,
        committed_at_unix_ms: 1,
    };
    let message = body.signing_bytes(limits()).unwrap();
    assert!(message.starts_with(b"alt-receipt-v1\0"));
    body.transaction_sequence = 2;
    assert_ne!(body.signing_bytes(limits()).unwrap(), message);
    body.publication.refs[0].force = true;
    assert!(body.signing_bytes(limits()).is_err());
}
#[test]
fn unknown_is_observation_and_cannot_be_persisted_as_a_terminal_decision() {
    assert!(serde_json::from_str::<RequestState>(r#"{"state":"unknown"}"#).is_err());
    let value = RequestObservation::Unknown {
        key: publication().key,
    };
    assert_eq!(
        serde_json::from_slice::<RequestObservation>(&serde_json::to_vec(&value).unwrap()).unwrap(),
        value
    );
    assert!(
        RequestState::Expired {
            digest: "1".repeat(64)
        }
        .terminal()
    );
    assert!(
        !RequestState::Ready {
            digest: "1".repeat(64)
        }
        .terminal()
    );
}

#[test]
fn chunks_cover_each_payload_exactly_without_overlaps_or_gaps() {
    let mut m = Manifest::empty(ObjectFormat::Sha1);
    m.entries = vec![entry('1')];
    m.entries[0].chunks[0].bytes = 256;
    m.entries[0].chunks.push(Chunk {
        offset: 256,
        bytes: 256,
        blake3: "2".repeat(64),
    });
    let original = m.id(limits()).unwrap();
    m.entries[0].chunks[1].offset = 255;
    assert!(m.id(limits()).is_err());
    m.entries[0].chunks[1].offset = 257;
    assert!(m.id(limits()).is_err());
    m.entries[0].chunks[1].offset = 256;
    m.entries[0].chunks[1].bytes = 255;
    assert!(m.id(limits()).is_err());
    m.entries[0].chunks[1].bytes = 256;
    m.entries[0].chunks[1].blake3 = "3".repeat(64);
    assert_ne!(m.id(limits()).unwrap(), original);
}

#[test]
fn delegation_and_agent_session_are_part_of_the_request_identity() {
    let mut p = publication();
    let initial = p.digest(limits()).unwrap();
    p.attribution.kind = ActorKind::Agent;
    assert!(p.digest(limits()).is_err());
    p.attribution.agent_session = Some("session-a".into());
    let agent = p.digest(limits()).unwrap();
    assert_ne!(agent, initial);
    p.attribution.agent_session = Some("session-b".into());
    assert_ne!(p.digest(limits()).unwrap(), agent);
    p.attribution.on_behalf_of = Some("account-b".into());
    assert!(p.digest(limits()).is_err());
    p.attribution.delegation_id = Some("delegation-a".into());
    let delegated = p.digest(limits()).unwrap();
    assert_ne!(delegated, agent);
    p.attribution.delegation_id = Some("delegation-b".into());
    assert_ne!(p.digest(limits()).unwrap(), delegated);
}
#[test]
fn negotiation_refuses_non_overlapping_versions_and_invalid_instance_binding() {
    let mut caps = Capabilities {
        protocol_min: 1,
        protocol_max: 3,
        installation_id: "instance-a".into(),
        runtime_generation: 1,
        deployment_mode: DeploymentMode::SelfHosted,
        object_formats: vec![ObjectFormat::Sha1],
        features: vec![Feature::Publications],
        limits: limits(),
    };
    assert_eq!(caps.negotiate(1, 2).unwrap(), 2);
    assert!(caps.negotiate(4, 5).is_err());
    assert!(caps.negotiate(2, 1).is_err());
    caps.runtime_generation = 0;
    assert!(caps.negotiate(1, 2).is_err());
}

#[test]
fn counters_round_trip_above_javascript_integer_precision_without_coercion() {
    let mut p = publication();
    p.key.scope.recovery_epoch = u64::MAX;
    let bytes = p.canonical_bytes(limits()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value["key"]["scope"]["recovery_epoch"],
        u64::MAX.to_string()
    );
    assert_eq!(Publication::decode(&bytes, limits()).unwrap(), p);
    for invalid in [
        serde_json::json!(1),
        serde_json::json!("01"),
        serde_json::json!("+1"),
        serde_json::json!("18446744073709551616"),
    ] {
        let mut bad = value.clone();
        bad["key"]["scope"]["recovery_epoch"] = invalid;
        assert!(Publication::decode(&serde_json::to_vec(&bad).unwrap(), limits()).is_err());
    }
}

#[test]
fn task_inventory_requires_one_explicit_bundle_and_capability() {
    let mut p = publication();
    let mut manifest = Manifest::empty(ObjectFormat::Sha1);
    p.content_manifest = manifest.id(limits()).unwrap();
    assert!(manifest.validate_publication(&p, limits()).is_err());
    manifest.entries.push(ContentEntry {
        kind: ContentKind::TaskBundle,
        blake3: "a".repeat(64),
        bytes: 0,
        git_oid: None,
        chunks: vec![],
    });
    p.content_manifest = manifest.id(limits()).unwrap();
    assert!(manifest.validate_publication(&p, limits()).is_ok());
    p.record_roots.clear();
    assert!(manifest.validate_publication(&p, limits()).is_err());
    p.record_roots = publication().record_roots;
    manifest.entries.push(ContentEntry {
        kind: ContentKind::TaskBundle,
        blake3: "b".repeat(64),
        bytes: 0,
        git_oid: None,
        chunks: vec![],
    });
    p.content_manifest = manifest.id(limits()).unwrap();
    assert!(manifest.validate_publication(&p, limits()).is_err());
    let mut capabilities = Capabilities {
        protocol_min: 1,
        protocol_max: 1,
        installation_id: "receiver".into(),
        runtime_generation: 1,
        deployment_mode: DeploymentMode::SelfHosted,
        object_formats: vec![ObjectFormat::Sha1],
        limits: limits(),
        features: vec![Feature::Publications, Feature::TaskRecords],
    };
    assert!(capabilities.require_task_bundles().is_err());
    capabilities.features.push(Feature::TaskBundlesV1);
    assert!(capabilities.require_task_bundles().is_ok());
}
