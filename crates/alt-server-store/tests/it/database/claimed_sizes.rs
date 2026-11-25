//! Content whose digest another request selected first, with a size that is not
//! its size and bytes that never came.
use super::*;

/// Begins a request that selects each of `contents` as an attachment one byte
/// longer than it is, and uploads nothing.
fn claim(f: &mut Fixture, request: &str, scope: &Scope, contents: &[(String, u64)]) -> Payload {
    let mut p = Payload::new(request, None);
    p.publication.key.scope = scope.clone();
    for (digest, bytes) in contents {
        // a manifest names a digest once, at one size
        if p.manifest
            .entries
            .iter()
            .any(|entry| &entry.blake3 == digest)
        {
            continue;
        }
        p.manifest.entries.push(ContentEntry {
            kind: ContentKind::Attachment,
            blake3: digest.clone(),
            bytes: bytes + 1,
            git_oid: None,
            chunks: vec![Chunk {
                offset: 0,
                bytes: bytes + 1,
                blake3: digest.clone(),
            }],
        });
    }
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    let begun = f
        .store
        .begin(&f.credential, &p.publication, &p.manifest, 3_600_000);
    begun.unwrap();
    p
}

fn sizes(p: &Payload) -> Vec<(String, u64)> {
    let entries = p.manifest.entries.iter();
    entries
        .map(|entry| (entry.blake3.clone(), entry.bytes))
        .collect()
}

/// the size and state recorded for each of `contents`, which are all stored at their sizes
fn stored(f: &Fixture, contents: &[(String, u64)]) {
    for (digest, bytes) in contents {
        let row = "SELECT bytes,state FROM alt_blobs WHERE repository='repository' AND blake3=$1";
        let row = f.client().query_one(row, &[digest]).unwrap();
        let found = (row.get::<_, i64>(0) as u64, row.get::<_, String>(1));
        assert_eq!(found, (*bytes, "stored".to_owned()), "{digest}");
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_size_claimed_for_content_that_never_came_holds_no_other_request_to_it() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    let contents = sizes(&first);
    let scope = first.publication.key.scope.clone();
    let claimed = claim(&mut f, "claimed", &scope, &contents);
    first.publish_new(&mut f.store, &f.credential);
    stored(&f, &contents);

    // the request that claimed has nothing of what it selected
    let key = &claimed.publication.key;
    let leased = f
        .store
        .acquire_publication(&f.credential, key, 30_000, 1000);
    assert!(leased.is_err());
    // and what is stored has its size: another one is refused as it was
    let late = Payload::new("late", Some(first.commit));
    let mut wrong = late.manifest.clone();
    wrong
        .entries
        .push(claimed.manifest.entries.last().unwrap().clone());
    let mut publication = late.publication.clone();
    publication.content_manifest = wrong.id(limits()).unwrap();
    let begun = f.store.begin(&f.credential, &publication, &wrong, 60_000);
    assert!(
        format!("{begun:?}").contains("content size collision"),
        "{begun:?}"
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_size_claimed_holds_no_request_that_stores_its_content_in_a_segment() {
    let mut f = Fixture::new();
    let p = super::registration::shared_payload();
    let contents = sizes(&p);
    claim(&mut f, "claimed", &p.publication.key.scope, &contents);
    super::segments::upload(&mut f, &p);
    stored(&f, &contents);
    p.publish(&mut f.store, &f.credential);
}
