use super::*;

fn chunk(bytes: &[u8], offset: u64) -> Chunk {
    Chunk {
        offset,
        bytes: bytes.len() as u64,
        blake3: alt_hash::blake3::hash(bytes).to_hex().to_string(),
    }
}
fn with_shared_roles(request: &str, parent: Option<ObjectId>) -> Payload {
    let mut p = Payload::new(request, parent);
    let raw = b"abcdef".to_vec();
    let attachment = ContentEntry {
        kind: ContentKind::Attachment,
        blake3: alt_hash::blake3::hash(&raw).to_hex().to_string(),
        bytes: raw.len() as u64,
        git_oid: None,
        chunks: vec![chunk(b"abc", 0), chunk(b"def", 3)],
    };
    let mut blob = attachment.clone();
    blob.kind = ContentKind::Blob;
    blob.git_oid = Some(ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &raw).to_string());
    let short = b"abc".to_vec();
    let shared_chunk = ContentEntry {
        kind: ContentKind::Attachment,
        blake3: alt_hash::blake3::hash(&short).to_hex().to_string(),
        bytes: short.len() as u64,
        git_oid: None,
        chunks: vec![chunk(&short, 0)],
    };
    p.manifest.entries.extend([attachment, blob, shared_chunk]);
    p.contents.extend([raw.clone(), raw, short]);
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    p
}
#[test]
#[ignore = "requires isolated PostgreSQL; shared root/chunk roles and original selection boundaries"]
fn indexed_selection_preserves_equal_bytes_in_multiple_roles_and_exact_chunks() {
    let mut f = Fixture::new();
    let p = with_shared_roles("shared-selection", None);
    let key = &p.publication.key;
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let full = &p.manifest.entries[2];
    assert_eq!(
        f.store
            .selected_upload(&f.credential, key, &full.blake3)
            .unwrap(),
        p.manifest.entries[2..4]
    );
    let only_chunk = chunk(b"def", 0);
    assert_eq!(
        f.store
            .selected_chunk(&f.credential, key, &only_chunk.blake3)
            .unwrap(),
        3
    );
    assert!(
        f.store
            .selected_upload(&f.credential, key, &only_chunk.blake3)
            .is_err()
    );
    assert!(
        f.store
            .selected_chunk(&f.credential, key, &full.blake3)
            .is_err()
    );
    let proof = f
        .store
        .bytes()
        .put(
            &key.scope,
            &full.blake3,
            full.bytes,
            limits().object_bytes,
            &mut Cursor::new(b"abcdef"),
        )
        .unwrap();
    let mut alternate = full.clone();
    alternate.chunks = vec![chunk(b"abcdef", 0)];
    assert!(matches!(
        f.store
            .register_content(&f.credential, key, &alternate, &proof),
        Err(alt_server_store::Error::Integrity(
            "upload scope or selection"
        ))
    ));
    for bytes in [b"abc", b"def"] {
        let digest = alt_hash::blake3::hash(bytes).to_hex().to_string();
        f.store
            .upload_chunk(&f.credential, key, &digest, &mut Cursor::new(bytes))
            .unwrap();
    }
    f.store
        .assemble_content(&f.credential, key, &full.blake3)
        .unwrap();
    let before: i64 = f
        .client()
        .query_one("SELECT stored_bytes FROM alt_repositories", &[])
        .unwrap()
        .get(0);
    f.store
        .assemble_content(&f.credential, key, &full.blake3)
        .unwrap();
    assert_eq!(
        f.client()
            .query_one("SELECT stored_bytes FROM alt_repositories", &[])
            .unwrap()
            .get::<_, i64>(0),
        before
    );
    assert_eq!(
        f.store
            .upload_status(&f.credential, key)
            .unwrap()
            .verified_chunks
            .len(),
        2
    );
    assert_eq!(f.counts(), (0, 0, 0));
    // Every current grant/expiry check still precedes indexed selection.
    f.store
        .set_grants(&f.credential, "repository", "owner", false, true, &grants())
        .unwrap();
    assert!(
        f.store
            .selected_upload(&f.credential, key, &full.blake3)
            .is_err()
    );
    assert!(
        f.store
            .selected_chunk(&f.credential, key, &only_chunk.blake3)
            .is_err()
    );
    f.store
        .set_grants(&f.credential, "repository", "owner", true, true, &grants())
        .unwrap();
    f.client()
        .batch_execute("UPDATE alt_requests SET expires_at=0")
        .unwrap();
    assert!(
        f.store
            .selected_upload(&f.credential, key, &full.blake3)
            .is_err()
    );
    assert!(
        f.store
            .register_content(&f.credential, key, full, &proof)
            .is_err()
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL; indexed equal-byte roles and chunk roots settle Cloud ingress once"]
fn indexed_selection_cloud_ingress_counts_shared_roots_once_and_chunks_only_when_roots() {
    use golia_platform_contract::Counter;
    let mut f = super::cloud::cloud();
    super::cloud::repository(&mut f);
    let p = with_shared_roles("cloud-shared-selection", None);
    let unique: std::collections::BTreeMap<_, _> = p
        .manifest
        .entries
        .iter()
        .map(|e| (&e.blake3, e.bytes))
        .collect();
    let bytes: u64 = unique.values().sum();
    let mut policy = super::cloud::service(3, 2, true);
    policy.limits.monthly_ingress_bytes = Counter(bytes);
    f.store.refresh_cloud_service(&policy).unwrap();
    f.store
        .refresh_cloud_account(
            &alt_server_store::cloud_credential_digest(&f.token).unwrap(),
            &super::cloud::claims(4, 2),
        )
        .unwrap();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    for raw in [b"abc", b"def"] {
        let digest = alt_hash::blake3::hash(raw).to_hex().to_string();
        f.store
            .upload_chunk(
                &f.credential,
                &p.publication.key,
                &digest,
                &mut Cursor::new(raw),
            )
            .unwrap();
    }
    assert_eq!(
        f.client()
            .query_one(
                "SELECT sum(accepted_bytes)::bigint FROM alt_cloud_ingress",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        3
    );
    f.store
        .assemble_content(
            &f.credential,
            &p.publication.key,
            &p.manifest.entries[2].blake3,
        )
        .unwrap();
    assert_eq!(
        f.client()
            .query_one(
                "SELECT sum(accepted_bytes)::bigint FROM alt_cloud_ingress",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        9
    );
    p.upload(&mut f.store, &f.credential);
    p.upload(&mut f.store, &f.credential);
    let row = f
        .client()
        .query_one(
            "SELECT sum(accepted_bytes)::bigint,sum(reserved_bytes)::bigint FROM alt_cloud_ingress",
            &[],
        )
        .unwrap();
    assert_eq!(
        (row.get::<_, i64>(0), row.get::<_, i64>(1)),
        (bytes as i64, 0)
    );
    p.publish(&mut f.store, &f.credential);
    assert_eq!(f.counts(), (1, 1, 1));
}
