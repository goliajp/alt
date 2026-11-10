use super::*;

fn chunked(request: &str) -> Payload {
    let mut payload = Payload::new(request, None);
    payload.manifest.entries[1].chunks = payload.contents[1]
        .chunks(31)
        .enumerate()
        .map(|(i, bytes)| Chunk {
            offset: (i * 31) as u64,
            bytes: bytes.len() as u64,
            blake3: alt_hash::blake3::hash(bytes).to_hex().to_string(),
        })
        .collect();
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload
}
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn chunks_resume_after_restart_verify_assembly_and_release_temporary_storage() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let payload = chunked("chunk-resume");
    let base = "/v1/repositories/repository/epochs/1/requests";
    let request = format!("{base}/owner/chunk-resume");
    let entry = &payload.manifest.entries[1];
    assert_eq!(
        server
            .json(
                "POST",
                base,
                &f.token,
                &alt_server::BeginRequest {
                    publication: payload.publication.clone(),
                    manifest: payload.manifest.clone(),
                }
            )
            .0,
        200
    );
    let reserved: i64 = f
        .client()
        .query_one(
            "SELECT reserved_bytes FROM alt_repositories WHERE id='repository'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(
        reserved as u64,
        entry.bytes * 2,
        "quota includes simultaneous chunks and assembled payload"
    );
    let assemble = format!("{request}/content/{}/assemble", entry.blake3);
    assert_eq!(server.command("POST", &assemble, &f.token).0, 409);
    let first = &entry.chunks[0];
    let first_path = format!("{request}/chunks/{}", first.blake3);
    assert_eq!(
        server
            .send("PUT", &first_path, Some(&f.token), &[0; 31])
            .status(),
        422
    );
    for _ in 0..2 {
        assert_eq!(
            server
                .send(
                    "PUT",
                    &first_path,
                    Some(&f.token),
                    &payload.contents[1][..31]
                )
                .status(),
            200
        );
    }
    assert_eq!(f.counts(), (0, 0, 0));
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 1000)
            .unwrap(),
        0
    );
    drop(server);
    let server = Running::start(&f);
    let (status, uploaded) = server.command("GET", &format!("{request}/uploads"), &f.token);
    assert_eq!(status, 200);
    assert_eq!(
        uploaded["verified_chunks"],
        serde_json::json!([first.blake3])
    );
    for chunk in &entry.chunks[1..] {
        let start = chunk.offset as usize;
        assert_eq!(
            server
                .send(
                    "PUT",
                    &format!("{request}/chunks/{}", chunk.blake3),
                    Some(&f.token),
                    &payload.contents[1][start..start + chunk.bytes as usize]
                )
                .status(),
            200
        );
    }
    assert_eq!(server.command("POST", &assemble, &f.token).0, 200);
    let empty = format!(
        "{request}/content/{}/assemble",
        payload.manifest.entries[0].blake3
    );
    assert_eq!(server.command("POST", &empty, &f.token).0, 200);
    let (status, result) = server.command("POST", &format!("{request}/publish"), &f.token);
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["result"]["state"], "committed");
    assert_eq!(f.counts(), (1, 1, 1));
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 1000)
            .unwrap(),
        entry.chunks.len()
    );
    let row = f
        .client()
        .query_one(
            "SELECT stored_bytes,reserved_bytes FROM alt_repositories WHERE id='repository'",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, i64>(0) as u64, entry.bytes);
    assert_eq!(row.get::<_, i64>(1), 0);
    assert_eq!(
        server
            .send(
                "GET",
                &format!(
                    "/v1/repositories/repository/epochs/1/content/{}",
                    entry.blake3
                ),
                Some(&f.token),
                b""
            )
            .into_string()
            .unwrap()
            .as_bytes(),
        payload.contents[1]
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn chunk_upload_rejects_unselected_scopes_and_revocation_before_assembly() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let payload = chunked("chunk-scope");
    let base = "/v1/repositories/repository/epochs/1/requests";
    let request = format!("{base}/owner/chunk-scope");
    assert_eq!(
        server
            .json(
                "POST",
                base,
                &f.token,
                &alt_server::BeginRequest {
                    publication: payload.publication.clone(),
                    manifest: payload.manifest.clone(),
                }
            )
            .0,
        200
    );
    let chunk = &payload.manifest.entries[1].chunks[0];
    for path in [
        format!("{request}/chunks/{}", "0".repeat(64)),
        format!("{base}/other/chunk-scope/chunks/{}", chunk.blake3),
        format!(
            "/v1/repositories/repository/epochs/2/requests/owner/chunk-scope/chunks/{}",
            chunk.blake3
        ),
    ] {
        assert_eq!(
            server
                .send("PUT", &path, Some(&f.token), &payload.contents[1][..31])
                .status(),
            403
        );
    }
    let path = format!("{request}/chunks/{}", chunk.blake3);
    assert_eq!(
        server
            .send("PUT", &path, Some(&f.token), &payload.contents[1][..32])
            .status(),
        413
    );
    assert_eq!(
        server
            .send("PUT", &path, Some(&f.token), &payload.contents[1][..31])
            .status(),
        200
    );
    f.store
        .set_grants(&f.credential, "repository", "owner", true, false, &[])
        .unwrap();
    assert_eq!(
        server
            .send("PUT", &path, Some(&f.token), &payload.contents[1][..31])
            .status(),
        403
    );
    assert_eq!(
        server
            .command("GET", &format!("{request}/uploads"), &f.token)
            .0,
        403
    );
    assert_eq!(f.counts(), (0, 0, 0));
}
