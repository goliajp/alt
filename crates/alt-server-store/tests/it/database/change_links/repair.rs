use super::*;
use ed25519_dalek::SigningKey;
use std::sync::{Arc, Barrier};

fn prepare(f: &mut Fixture) -> (ChangeChannel, link::SignedEvent, link::RepairSource) {
    native(f);
    let channel = setup(f);
    let signed = repair(f, &channel, 1, None, "refs/heads/main");
    receive(f, &channel, &signed).unwrap();
    let source = f
        .store
        .repair_task_source(&f.credential, &scope(f), &signed.digest().unwrap())
        .unwrap();
    (channel, signed, source)
}
fn ready(
    f: &mut Fixture,
    payload: &Payload,
) -> (
    alt_server_store::PublicationWork,
    alt_repository_service::VerifiedGraph,
    alt_task_service::VerifiedTaskGraph,
) {
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let (graph, tasks) = super::super::tasks::verified(&work, payload.commit);
    f.store
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    (work, graph, tasks)
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn concurrent_native_publications_bind_exactly_one_task_without_claiming_execution() {
    let mut f = Fixture::new();
    let (_, signed, source) = prepare(&mut f);
    let digest = signed.digest().unwrap();
    assert!(
        f.store
            .repair_task_claim(&f.credential, &scope(&f), &digest)
            .unwrap()
            .is_none()
    );
    let before = f.counts();
    let barrier = Arc::new(Barrier::new(2));
    let mut threads = vec![];
    for name in ["first-client", "second-client"] {
        let payload = tasks::task_payload_with_source(name, Some(source.clone()));
        payload.upload(&mut f.store, &f.credential);
        let mut store = f.reopen();
        let credential = f.credential.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            let work = store
                .acquire_publication(&credential, &payload.publication.key, 60_000, 1000)
                .unwrap();
            let (graph, tasks) = tasks::verified(&work, payload.commit);
            store
                .mark_ready_with_tasks(&credential, &work, &graph, &tasks)
                .unwrap();
            barrier.wait();
            let result = store.publish_with_tasks(&credential, &work, &graph, &tasks);
            (payload.publication.record_roots[0].id.clone(), result)
        }));
    }
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|(_, r)| r.is_ok()).count(), 1);
    let winner = results.iter().find(|(_, r)| r.is_ok()).unwrap();
    assert!(
        results
            .iter()
            .filter_map(|(_, r)| r.as_ref().err())
            .all(|e| e.to_string().contains("already bound"))
    );
    let claim = f
        .store
        .repair_task_claim(&f.credential, &scope(&f), &digest)
        .unwrap()
        .unwrap();
    assert_eq!(claim.task, winner.0);
    assert_eq!(
        claim.publication_digest,
        winner
            .1
            .as_ref()
            .unwrap()
            .body
            .publication
            .digest(limits())
            .unwrap()
    );
    assert_eq!(f.counts(), (before.0, before.1 + 1, before.2 + 1));
    let row=f.client().query_one("SELECT (SELECT count(*) FROM alt_task_records),(SELECT count(*) FROM alt_link_task_claims),(SELECT count(*) FROM alt_link_outbox)",&[]).unwrap();
    assert_eq!(
        (
            row.get::<_, i64>(0),
            row.get::<_, i64>(1),
            row.get::<_, i64>(2)
        ),
        (1, 1, 0)
    );
    // A database reconnect recovers the same claim. There is no Candidate,
    // Evidence, Review or Delivery manufactured by source acceptance/claiming.
    f.store = f.reopen();
    assert_eq!(
        f.store
            .repair_task_claim(&f.credential, &scope(&f), &digest)
            .unwrap(),
        Some(claim)
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn repair_publication_rechecks_current_pairing_and_exact_original_source() {
    let mut f = Fixture::new();
    let (c, signed, source) = prepare(&mut f);
    let payload = tasks::task_payload_with_source("revoked-before-commit", Some(source));
    let (work, graph, tasks) = ready(&mut f, &payload);
    let before = f.counts();
    f.store
        .disable_change_channel(&f.credential, &scope(&f), &c.channel)
        .unwrap();
    assert!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .is_err()
    );
    assert_eq!(f.counts(), before);
    assert!(
        f.store
            .repair_task_claim(&f.credential, &scope(&f), &signed.digest().unwrap())
            .unwrap()
            .is_none()
    );
    assert!(
        f.store
            .repair_task_source(&f.credential, &scope(&f), &signed.digest().unwrap())
            .is_err()
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn superseded_and_self_signed_repair_requests_cannot_acquire_native_claims() {
    let mut f = Fixture::new();
    let (c, signed, source) = prepare(&mut f);
    let payload = tasks::task_payload_with_source("superseded-before-commit", Some(source.clone()));
    let (work, graph, tasks) = ready(&mut f, &payload);
    let next = repair(&f, &c, 2, Some(signed.digest().unwrap()), "refs/heads/main");
    receive(&mut f, &c, &next).unwrap();
    assert!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .unwrap_err()
            .to_string()
            .contains("superseded")
    );
    let mut forged = source;
    let attacker = SigningKey::from_bytes(&[9; 32]);
    forged.request = link::SignedEvent::sign(forged.request.event.clone(), &attacker).unwrap();
    forged.source_public_key = alt_provenance::encode_hex(attacker.verifying_key().as_bytes());
    forged.acceptance =
        link::Receipt::accept(&forged.request, link::Counter(300), &attacker).unwrap();
    forged.receiver_public_key = forged.source_public_key.clone();
    forged.validate().unwrap();
    let payload = tasks::task_payload_with_source("unregistered-signatures", Some(forged));
    let (work, graph, tasks) = ready(&mut f, &payload);
    assert!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .is_err()
    );
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_link_task_claims", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn published_repair_history_replays_after_disable_without_installing_current_authority() {
    let mut f = Fixture::new();
    let (c, signed, source) = prepare(&mut f);
    let mut payload = tasks::task_payload_with_source("original-task", Some(source));
    let (work, graph, tasks) = ready(&mut f, &payload);
    let receipt = f
        .store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    let claim = f
        .store
        .repair_task_claim(&f.credential, &scope(&f), &signed.digest().unwrap())
        .unwrap()
        .unwrap();
    f.store
        .disable_change_channel(&f.credential, &scope(&f), &c.channel)
        .unwrap();
    assert_eq!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .unwrap(),
        receipt
    );
    payload.publication.key.request_id = "republish-history".into();
    let (work, graph, tasks) = ready(&mut f, &payload);
    f.store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    assert_eq!(
        f.store
            .repair_task_claim(&f.credential, &scope(&f), &signed.digest().unwrap())
            .unwrap(),
        Some(claim)
    );
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_link_task_claims", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn task_binding_notification_requires_committed_claim_and_cannot_undo_it_on_quota_failure() {
    let mut f = Fixture::new();
    let (c, signed, source) = prepare(&mut f);
    let digest = signed.digest().unwrap();
    let request = alt_server_store::RepairBindingRequest {
        key: RequestKey {
            scope: scope(&f),
            actor_id: "owner".into(),
            request_id: "notify-task".into(),
        },
        channel: c.channel.clone(),
        configuration_digest: c.configuration_digest.clone(),
        repair_request_digest: digest.clone(),
    };
    assert!(
        f.store
            .enqueue_repair_binding(&f.credential, &request)
            .is_err()
    );
    let payload = tasks::task_payload_with_source("published-task", Some(source));
    let (work, graph, tasks) = ready(&mut f, &payload);
    let receipt = f
        .store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    let claim = f
        .store
        .repair_task_claim(&f.credential, &scope(&f), &digest)
        .unwrap()
        .unwrap();
    let before = f.counts();
    f.client()
        .batch_execute("UPDATE alt_repositories SET quota_bytes=stored_bytes+link_bytes")
        .unwrap();
    assert!(matches!(
        f.store.enqueue_repair_binding(&f.credential, &request),
        Err(alt_server_store::Error::Limit(_))
    ));
    assert_eq!(f.counts(), before);
    assert_eq!(
        f.store
            .repair_task_claim(&f.credential, &scope(&f), &digest)
            .unwrap(),
        Some(claim.clone())
    );
    assert_eq!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .unwrap(),
        receipt
    );
    f.client()
        .batch_execute("UPDATE alt_repositories SET quota_bytes=100000000")
        .unwrap();
    let outgoing = f
        .store
        .enqueue_repair_binding(&f.credential, &request)
        .unwrap();
    assert_eq!(
        outgoing.statement.event.body,
        link::Body::TaskBound {
            repair_request_digest: digest,
            task: claim.task,
            publication_digest: claim.publication_digest
        }
    );
    assert_eq!(
        f.store
            .enqueue_repair_binding(&f.credential, &request)
            .unwrap(),
        outgoing
    );
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_link_outbox", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL and actual native Task publications"]
fn bounded_claim_and_history_pages_visit_unannounced_work_without_repeating_rows() {
    let mut f = Fixture::new();
    native(&mut f);
    let c = setup(&mut f);
    let s = scope(&f);
    let mut claims = Vec::new();
    for index in 0..51 {
        let mut event = repair(&f, &c, 1, None, "refs/heads/main").event;
        event.entity = format!("repair-page-{index}");
        let signed = link::SignedEvent::sign(event, &SigningKey::from_bytes(&[4; 32])).unwrap();
        receive(&mut f, &c, &signed).unwrap();
        if index >= 17 {
            continue;
        }
        let digest = signed.digest().unwrap();
        let source = f
            .store
            .repair_task_source(&f.credential, &s, &digest)
            .unwrap();
        let payload = tasks::task_payload_with_source(&format!("page-task-{index}"), Some(source));
        let (work, graph, tasks) = ready(&mut f, &payload);
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .unwrap();
        claims.push(digest);
    }
    claims.sort();
    let first = f
        .store
        .unannounced_repair_tasks(&f.credential, &s, &c.channel, &c.configuration_digest, None)
        .unwrap();
    assert_eq!(first, claims[..16]);
    // None of the first page is announced: the next cursor must still make
    // progress so a blocked notification cannot hide later independent work.
    let second = f
        .store
        .unannounced_repair_tasks(
            &f.credential,
            &s,
            &c.channel,
            &c.configuration_digest,
            first.last().map(String::as_str),
        )
        .unwrap();
    assert_eq!(second, claims[16..]);
    let request = alt_server_store::RepairBindingRequest {
        key: RequestKey {
            scope: s.clone(),
            actor_id: "owner".into(),
            request_id: "last-page-notify".into(),
        },
        channel: c.channel.clone(),
        configuration_digest: c.configuration_digest.clone(),
        repair_request_digest: second[0].clone(),
    };
    let outgoing = f
        .store
        .enqueue_repair_binding(&f.credential, &request)
        .unwrap();
    assert!(
        f.store
            .unannounced_repair_tasks(
                &f.credential,
                &s,
                &c.channel,
                &c.configuration_digest,
                first.last().map(String::as_str)
            )
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.store
            .pending_change_page(&f.credential, &s, &c.channel, &c.configuration_digest, None)
            .unwrap(),
        vec![outgoing.statement.digest().unwrap()]
    );
    let page = f.store.change_events(&f.credential, &s, None).unwrap();
    assert_eq!(page.entries.len(), 50);
    let next = f
        .store
        .change_events(&f.credential, &s, page.next.as_deref())
        .unwrap();
    assert_eq!(next.entries.len(), 2);
    assert!(next.next.is_none());
    let unique: std::collections::BTreeSet<_> = page
        .entries
        .iter()
        .chain(&next.entries)
        .map(|e| &e.digest)
        .collect();
    assert_eq!(unique.len(), 52);
    assert!(
        f.store
            .unannounced_repair_tasks(
                &f.credential,
                &s,
                &c.channel,
                &c.configuration_digest,
                Some("invalid")
            )
            .is_err()
    );
    f.store
        .disable_change_channel(&f.credential, &s, &c.channel)
        .unwrap();
    assert!(
        f.store
            .pending_change_page(&f.credential, &s, &c.channel, &c.configuration_digest, None)
            .is_err()
    );
    assert!(
        f.store
            .unannounced_repair_tasks(&f.credential, &s, &c.channel, &c.configuration_digest, None)
            .is_err()
    );
    assert_eq!(
        f.store
            .change_events(&f.credential, &s, None)
            .unwrap()
            .entries
            .len(),
        50
    );
}
