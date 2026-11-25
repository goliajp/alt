//! What a native publication is given of the objects the segments hold.
use super::*;
use alt_wire_http::native::{Options, Transport};

pub(in crate::database) fn named(
    request: &str,
    name: &str,
    parent: Option<ObjectId>,
    commit: &str,
) -> Payload {
    let mut payload = Payload::new(commit, parent);
    payload.publication.key.request_id = request.into();
    payload.publication.refs[0].name = name.into();
    payload.publication.refs[0].expected = None;
    payload
}

pub(in crate::database) fn without_content(mut payload: Payload) -> Payload {
    payload.manifest.entries.clear();
    payload.contents.clear();
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload
}

pub(in crate::database) fn begun(client: &Transport, payload: &Payload) {
    let request = BeginRequest {
        publication: payload.publication.clone(),
        manifest: payload.manifest.clone(),
    };
    let state = client.begin(&request, limits()).unwrap();
    assert!(matches!(state, RequestState::Receiving { .. }));
}

pub(in crate::database) fn brought(client: &Transport, payload: &Payload) {
    let key = &payload.publication.key;
    for (entry, bytes) in payload.manifest.entries.iter().zip(&payload.contents) {
        for chunk in &entry.chunks {
            client.upload_chunk(key, chunk, bytes).unwrap();
        }
        client.assemble(key, &entry.blake3).unwrap();
    }
}

/// a client of the native protocol with the token of the owner
pub(in crate::database) fn client(server: &Running, token: &str) -> Transport {
    let options = Options {
        timeout: Duration::from_secs(15),
        response_bytes: 1_000_000,
        chunk_bytes: 1_000_000,
        content_bytes: 100_000_000,
        allow_loopback_http: true,
    };
    Transport::new(&server.url, token.to_owned(), options).unwrap()
}

/// whether the server published the request, begun and brought to it
pub(in crate::database) fn sent(client: &Transport, payload: &Payload) -> bool {
    begun(client, payload);
    brought(client, payload);
    published(client, payload)
}

/// whether the server published the request once it was asked to
fn published(client: &Transport, payload: &Payload) -> bool {
    let key = &payload.publication.key;
    let _ = client.prepare(key);
    match client.publish(key) {
        Ok(RequestObservation::Recorded { result, .. }) => {
            matches!(*result, RequestState::Committed { .. })
        }
        _ => false,
    }
}

/// Through the server: a native publication that names what a deleted branch
/// left in the segments, by its id, by a parent or by the digest of its
/// content, is not published; one that brings it is, and the refs lead to it
/// from then on.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_native_publication_refers_to_what_the_refs_lead_to_or_brings_it() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let client = client(&server, &f.token);
    let sent = |payload: &Payload| sent(&client, payload);
    let gone = Payload::new("the-branch-that-goes", None);
    assert!(sent(&gone));
    let mut deletion = without_content(Payload::new("remove-it", Some(gone.commit)));
    deletion.publication.refs[0].new = None;
    assert!(sent(&deletion));
    let kept = Payload::new("the-branch-that-stays", None);
    assert!(sent(&kept));
    assert_eq!(f.counts().0, 1);

    let commit = "the-branch-that-goes";
    let by_id = without_content(named("by-its-id", "refs/heads/back", None, commit));
    assert_eq!(by_id.commit, gone.commit);
    assert!(!sent(&by_id));
    let child = |request| named(request, "refs/heads/child", Some(gone.commit), "a-child");
    assert!(!sent(&child("by-a-parent")));
    // named by the digest of its content and not sent: the server says it waits for it
    let by_digest = named("by-its-digest", "refs/heads/back", None, commit);
    begun(&client, &by_digest);
    let key = &by_digest.publication.key;
    let waited_for = client.upload_status(key).unwrap().verified_chunks;
    assert!(!waited_for.contains(&by_digest.manifest.entries[1].blake3));
    assert!(!published(&client, &by_digest));
    assert_eq!(f.counts().0, 1);

    brought(&client, &by_digest);
    assert!(published(&client, &by_digest));
    let again = without_content(named("by-its-id-again", "refs/heads/again", None, commit));
    assert!(sent(&again));
    assert!(sent(&child("by-a-parent-again")));
    assert_eq!(f.counts().0, 4);
}
