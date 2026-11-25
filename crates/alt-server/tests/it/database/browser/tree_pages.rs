//! A directory is listed a page at a time.
use super::*;

/// A published commit whose directory `wide` has `entries` files; its id.
fn wide(f: &Fixture, server: &Running, entries: usize) -> String {
    let mut p = Payload::new("wide", None);
    let blob = b"the same in every file\n".to_vec();
    let blob_oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &blob);
    let mut inner = Vec::new();
    for n in 0..entries {
        inner.extend_from_slice(format!("100644 f{n:04}\0").as_bytes());
        inner.extend_from_slice(blob_oid.as_bytes());
    }
    let inner_oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &inner);
    let mut root = b"40000 wide\0".to_vec();
    root.extend_from_slice(inner_oid.as_bytes());
    let root_oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &root);
    let commit = format!("tree {root_oid}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\na wide directory\n").into_bytes();
    p.commit = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit);
    p.contents = vec![blob, inner, root, commit];
    let kinds = [
        (ContentKind::Blob, blob_oid),
        (ContentKind::Tree, inner_oid),
        (ContentKind::Tree, root_oid),
        (ContentKind::Commit, p.commit),
    ];
    p.manifest.entries = (p.contents.iter().zip(kinds))
        .map(|(bytes, (kind, oid))| {
            let digest = alt_hash::blake3::hash(bytes).to_hex().to_string();
            ContentEntry {
                kind,
                blake3: digest.clone(),
                bytes: bytes.len() as u64,
                git_oid: Some(oid.to_string()),
                chunks: vec![Chunk {
                    offset: 0,
                    bytes: bytes.len() as u64,
                    blake3: digest,
                }],
            }
        })
        .collect();
    p.publication.refs[0].new = Some(p.commit.to_string());
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    let base = "/v1/repositories/repository/epochs/1/requests";
    let begin = alt_server::BeginRequest {
        publication: p.publication.clone(),
        manifest: p.manifest.clone(),
    };
    assert_eq!(server.json("POST", base, &f.token, &begin).0, 200);
    let request = format!("{base}/owner/wide");
    for (entry, bytes) in p.manifest.entries.iter().zip(&p.contents) {
        let content = format!("{request}/content/{}", entry.blake3);
        let sent = server.send("PUT", &content, Some(&f.token), bytes);
        assert_eq!(sent.status(), 200);
    }
    let published = server.command("POST", &format!("{request}/publish"), &f.token);
    assert_eq!(published.0, 200, "{}", published.1);
    p.commit.to_string()
}

/// Seven hundred files in one directory come in two pages, the second from where
/// the first says it begins; each file is in one of them, in the order of the
/// directory. The page after the last is empty, and none is past the end.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn a_directory_of_more_entries_than_a_page_holds_is_read_page_after_page() {
    let mut f = Fixture::new();
    user(&mut f, "reader", false);
    f.store
        .set_grants(&f.credential, "repository", "reader", true, false, &[])
        .unwrap();
    let server = start(&mut f);
    let (cookie, _) = login(&server, "reader");
    let commit = wide(&f, &server, 700);
    let page = |path: &str, after: &str| {
        let tree = format!("/web/repositories/repository/tree?commit={commit}&path={path}{after}");
        get(&server, &tree, &cookie)
    };
    let names = |page: &Value| -> Vec<String> {
        (page["entries"].as_array().unwrap().iter())
            .map(|entry| entry["name"].as_str().unwrap().to_owned())
            .collect()
    };

    let (status, root) = page("", "");
    assert_eq!(status, 200, "{root}");
    assert_eq!(names(&root), ["wide"]);
    assert_eq!(root["next"], Value::Null);
    let wide = root["entries"][0]["path"].as_str().unwrap();

    let (status, first) = page(wide, "");
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["next"], "500");
    assert_eq!(page(wide, "&after=0").1, first);
    let (status, second) = page(wide, "&after=500");
    assert_eq!(status, 200, "{second}");
    assert_eq!(second["next"], Value::Null);
    let all: Vec<String> = (0..700).map(|n| format!("f{n:04}")).collect();
    assert_eq!([names(&first), names(&second)].concat(), all);
    assert_eq!(names(&first).len(), 500);

    let (status, end) = page(wide, "&after=700");
    assert_eq!(status, 200, "{end}");
    assert_eq!((names(&end).len(), &end["next"]), (0, &Value::Null));
    assert_eq!(page(wide, "&after=701").0, 409);
    assert_eq!(page(wide, "&after=x").0, 409);
}
