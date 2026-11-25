use super::*;

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn task_publication_preserves_baseline_after_ref_deletion() {
    let mut f = Fixture::new();
    let old = Payload::new("historical-task-baseline", None);
    old.upload(&mut f.store, &f.credential);
    old.publish(&mut f.store, &f.credential);
    let mut payload = task_payload("historical-task-baseline");
    payload.publication.key.request_id = "publish-task-baseline".into();
    assert_eq!(payload.commit, old.commit);
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let (graph, tasks) = verified(&work, payload.commit);
    f.store
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    f.store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    let mut deletion = Payload::new("remove-current-ref", Some(old.commit));
    deletion.publication.refs[0].new = None;
    deletion.manifest.entries.clear();
    deletion.contents.clear();
    deletion.publication.content_manifest = deletion.manifest.id(limits()).unwrap();
    deletion.upload(&mut f.store, &f.credential);
    deletion.publish(&mut f.store, &f.credential);
    assert_eq!(f.counts().0, 0);
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    let (_, mut file) = f
        .store
        .open_published_content(
            &f.credential,
            &payload.publication.key.scope,
            &old.manifest.entries[1].blake3,
        )
        .expect("task baseline must survive current ref deletion");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
    assert_eq!(bytes, old.contents[1]);
}

/// The ref `main` deleted, where it is at `at`.
fn deleted(f: &mut Fixture, request: &str, at: ObjectId) {
    let mut deletion = Payload::new(request, Some(at));
    deletion.publication.refs[0].new = None;
    deletion.manifest.entries.clear();
    deletion.contents.clear();
    deletion.publication.content_manifest = deletion.manifest.id(limits()).unwrap();
    deletion.upload(&mut f.store, &f.credential);
    deletion.publish(&mut f.store, &f.credential);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn what_a_deleted_ref_led_to_is_given_where_a_published_record_names_it_and_not_otherwise() {
    let mut f = Fixture::new();
    let scope = Payload::new("scope", None).publication.key.scope;
    let commit = |payload: &Payload| payload.manifest.entries[1].blake3.clone();
    let given = |f: &mut Fixture, digest: &str| {
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap();
        match f
            .store
            .open_published_content(&f.credential, &scope, digest)
        {
            Ok((_, mut file)) => {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
                Some(bytes)
            }
            Err(alt_server_store::Error::Unauthorized) => None,
            Err(error) => panic!("{error}"),
        }
    };
    // a commit no record names, given while the ref leads to it
    let unnamed = Payload::new("no-record-names-this", None);
    unnamed.publish_new(&mut f.store, &f.credential);
    assert_eq!(
        given(&mut f, &commit(&unnamed)),
        Some(unnamed.contents[1].clone())
    );
    deleted(&mut f, "remove-the-unnamed", unnamed.commit);
    assert_eq!(given(&mut f, &commit(&unnamed)), None);
    // a commit under the ref anew is given by the ref, the other still is not
    let named = Payload::new("a-task-names-this", None);
    named.publish_new(&mut f.store, &f.credential);
    assert_eq!(
        given(&mut f, &commit(&named)),
        Some(named.contents[1].clone())
    );
    assert_eq!(given(&mut f, &commit(&unnamed)), None);
    // its ref deleted before any record names it, it is not given either
    deleted(&mut f, "remove-the-named", named.commit);
    assert_eq!(given(&mut f, &commit(&named)), None);
    // brought again, a task published with it as its baseline, and the ref deleted again
    let mut back = Payload::new("a-task-names-this", None);
    back.publication.key.request_id = "the-named-back".into();
    assert_eq!(back.commit, named.commit);
    back.publish_new(&mut f.store, &f.credential);
    let mut payload = task_payload("a-task-names-this");
    payload.publication.key.request_id = "publish-the-task".into();
    assert_eq!(payload.commit, named.commit);
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let (graph, tasks) = verified(&work, payload.commit);
    f.store
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    f.store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    deleted(&mut f, "remove-the-named-again", named.commit);
    assert_eq!(
        given(&mut f, &commit(&named)),
        Some(named.contents[1].clone())
    );
    assert_eq!(given(&mut f, &commit(&unnamed)), None);
    // one shown the repository by its refs alone is given neither
    for payload in [&named, &unnamed] {
        let reached = f
            .store
            .open_reached_content(&f.credential, &scope, &commit(payload), None);
        assert!(matches!(
            reached,
            Err(alt_server_store::Error::Unauthorized)
        ));
    }
}

/// What the refs of a repository lead to is found by reading every tree they
/// reach. It is kept for whoever asks it of the same refs, and found anew once
/// the refs lead to nothing of what they led to; and it is looked for no longer
/// than the one who asked waits for it, which one who finds it kept does not
/// notice.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn what_the_refs_lead_to_is_kept_and_looked_for_no_longer_than_it_is_waited_for() {
    use std::sync::{Arc, atomic::AtomicBool};
    let mut f = Fixture::new();
    let scope = Payload::new("scope", None).publication.key.scope;
    let gone_one = Payload::new("the-branch-that-goes", None);
    gone_one.publish_new(&mut f.store, &f.credential);
    deleted(&mut f, "remove-it", gone_one.commit);
    let kept = Payload::new("the-branch-that-stays", None);
    kept.publish_new(&mut f.store, &f.credential);
    let steps = f.store.limits().walk_steps;
    let (gone, waited) = (AtomicBool::new(true), AtomicBool::new(false));
    let view = |f: &mut Fixture| f.store.pack_view(&f.credential, "repository").unwrap();
    let digest = &kept.manifest.entries[1].blake3;

    // nobody waits: neither for the objects, nor for one of them by its digest
    let busy = |found: std::result::Result<(), alt_server_store::Error>| {
        assert!(matches!(found, Err(alt_server_store::Error::Busy(_))));
    };
    busy(view(&mut f).led(steps, Some(&gone)).map(drop));
    busy(
        (f.store)
            .open_reached_content(&f.credential, &scope, digest, Some(&gone))
            .map(drop),
    );
    let first = view(&mut f).led(steps, Some(&waited)).unwrap();
    assert!(first.to(kept.commit.as_bytes().try_into().unwrap()));
    assert!(!first.to(gone_one.commit.as_bytes().try_into().unwrap()));
    // kept: the same answer, and nothing that could be given up
    let again = view(&mut f).led(steps, Some(&gone)).unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    (f.store)
        .open_reached_content(&f.credential, &scope, digest, Some(&gone))
        .unwrap();
    // the branch is gone and another is there that does not lead to it: found anew
    deleted(&mut f, "remove-the-other", kept.commit);
    let back = Payload::new("the-branch-that-comes", None);
    back.publish_new(&mut f.store, &f.credential);
    busy(view(&mut f).led(steps, Some(&gone)).map(drop));
    let anew = view(&mut f).led(steps, None).unwrap();
    assert!(!Arc::ptr_eq(&first, &anew));
}
