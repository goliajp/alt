use super::*;
use alt_repository_service::{Error as Graph, verify_new_graph};

type Read = std::result::Result<alt_server_store::PublicationWork, Graph>;

fn named(request: &str, name: &str, parent: Option<ObjectId>, commit: &str) -> Payload {
    let mut payload = Payload::new(commit, parent);
    payload.publication.key.request_id = request.into();
    payload.publication.refs[0].name = name.into();
    payload.publication.refs[0].expected = None;
    payload
}

fn without_content(mut payload: Payload) -> Payload {
    payload.manifest.entries.clear();
    payload.contents.clear();
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload
}

/// the graph of a request as the server reads it before it publishes
fn read(f: &mut Fixture, payload: &Payload) -> Read {
    let key = &payload.publication.key;
    let work = (f.store)
        .acquire_publication(&f.credential, key, 30_000, 1000)
        .unwrap();
    let roots = work.roots().unwrap();
    verify_new_graph(work.source(), HashAlgo::Sha1, &roots, graph_limits())?;
    Ok(work)
}

/// A publication is given the objects of the segments that the refs lead to.
/// What a deleted branch left in them it has to bring, as if the repository
/// never had it: named by its id alone, by a parent, or by the digest of
/// its content, it is not there.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_publication_refers_to_what_the_refs_lead_to_or_brings_it() {
    let mut f = Fixture::new();
    let gone = Payload::new("the-branch-that-goes", None);
    gone.publish_new(&mut f.store, &f.credential);
    let mut deletion = without_content(Payload::new("remove-it", Some(gone.commit)));
    deletion.publication.refs[0].new = None;
    deletion.publish_new(&mut f.store, &f.credential);
    let kept = Payload::new("the-branch-that-stays", None);
    kept.publish_new(&mut f.store, &f.credential);

    // a ref pointed at it, with nothing brought
    let by_id = without_content(named(
        "by-its-id",
        "refs/heads/back",
        None,
        "the-branch-that-goes",
    ));
    assert_eq!(by_id.commit, gone.commit);
    by_id.upload(&mut f.store, &f.credential);
    let missing = |read: Read, oid| match read {
        Err(Graph::Missing(missing)) => assert_eq!(missing, oid),
        Err(error) => panic!("{error}"),
        Ok(_) => panic!("the publication is given what no ref leads to"),
    };
    missing(read(&mut f, &by_id), gone.commit);
    // a commit brought whose parent it is
    let child = named(
        "by-a-parent",
        "refs/heads/child",
        Some(gone.commit),
        "a-child",
    );
    child.upload(&mut f.store, &f.credential);
    missing(read(&mut f, &child), gone.commit);
    // named by the digest of its content, and not sent: the request waits for it
    let by_digest = named(
        "by-its-digest",
        "refs/heads/back",
        None,
        "the-branch-that-goes",
    );
    (f.store)
        .begin(
            &f.credential,
            &by_digest.publication,
            &by_digest.manifest,
            60_000,
        )
        .unwrap();
    let key = &by_digest.publication.key;
    let unsent = f
        .store
        .acquire_publication(&f.credential, key, 30_000, 1000);
    assert!(matches!(
        unsent,
        Err(alt_server_store::Error::Precondition(
            "selected content not uploaded or published"
        ))
    ));
    assert_eq!(f.counts().0, 1);

    // brought, it is published; and from then on the refs lead to it
    by_digest.publish_new(&mut f.store, &f.credential);
    let after = without_content(named(
        "by-its-id-again",
        "refs/heads/again",
        None,
        "the-branch-that-goes",
    ));
    after.publish_new(&mut f.store, &f.credential);
    let child = named(
        "by-a-parent-again",
        "refs/heads/child",
        Some(gone.commit),
        "a-child",
    );
    child.publish_new(&mut f.store, &f.credential);
    assert_eq!(f.counts().0, 4);
    let view = f.store.pack_view(&f.credential, "repository").unwrap();
    let led = view.led(f.store.limits().walk_steps, None).unwrap();
    for commit in [gone.commit, kept.commit, child.commit] {
        assert!(led.to(commit.as_bytes().try_into().unwrap()));
    }
}
