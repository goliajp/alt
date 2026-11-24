//! What a reviewer reads of a request that waits for review, while its publisher
//! holds the request and a push holds the row of its repository.
use super::tasks::{current_reviewer, integration_payload, ready_integration, task_limits};
use super::*;
use alt_server_store::PublicationWork;
use alt_task_service::VerifiedTaskGraph;

fn verified(work: &PublicationWork) -> (alt_repository_service::VerifiedGraph, VerifiedTaskGraph) {
    let roots = work.roots().unwrap();
    let graph = verify_graph(work.source(), HashAlgo::Sha1, &roots, graph_limits()).unwrap();
    let tasks = VerifiedTaskGraph::verify(
        work.publication(),
        work.manifest(),
        limits(),
        &graph,
        work.source(),
        work.artifacts(),
        task_limits(),
    )
    .unwrap();
    (graph, tasks)
}

/// The four reads of a reviewer, each told as it is answered.
fn reads(
    mut reader: Store,
    reviewer: Credential,
    (key, integration, digest): (RequestKey, String, String),
    tell: std::sync::mpsc::Sender<String>,
) {
    let told = |name: &str, answer: String| tell.send(format!("{name}: {answer}")).unwrap();
    let selection = reader.review_selection(&reviewer, &key);
    told(
        "selection",
        alt_json::to_string(&selection.unwrap()).unwrap(),
    );
    let content = reader.open_review_content(&reviewer, &key, &digest);
    told("content", format!("{:?}", content.map(|found| found.0)));
    let work = reader.read_review(&reviewer, &key, 1000);
    told("request", format!("{:?}", work.as_ref().map(|_| ())));
    let work = work.unwrap();
    let (graph, tasks) = verified(&work);
    let now = reader.inspect_current_integration(&reviewer, &work, &graph, &tasks, &integration);
    told("integration", alt_json::to_string(&now.unwrap()).unwrap());
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_reviewer_reads_a_request_its_publisher_holds_and_holds_it_from_nobody() {
    let mut f = Fixture::new();
    let base = Payload::new("review-base", None);
    base.upload(&mut f.store, &f.credential);
    base.publish(&mut f.store, &f.credential);
    let data = integration_payload(&base, false, false);
    let (_, reviewer) = current_reviewer(&mut f, &data);
    let key = data.payload.publication.key.clone();
    // the publisher holds the lease of the request from here on
    let (held, graph, tasks) = ready_integration(&mut f, &data);
    let record = |entry: &&ContentEntry| entry.kind == ContentKind::Record;
    let digest = data.payload.manifest.entries.iter().find(record).unwrap();
    let asked = (key.clone(), data.integration.clone(), digest.blake3.clone());
    let wait = std::time::Duration::from_secs(20);
    let answers = |f: &Fixture| {
        let (reader, reviewer, asked) = (f.reopen(), reviewer.clone(), asked.clone());
        let (tell, told) = std::sync::mpsc::channel();
        let reading = std::thread::spawn(move || reads(reader, reviewer, asked, tell));
        let answers: Vec<_> = (0..4).map(|_| told.recv_timeout(wait).ok()).collect();
        (answers, reading)
    };
    let (leased, reading) = answers(&f);
    reading.join().unwrap();
    assert!(leased.iter().all(Option::is_some), "{leased:?}");
    assert!(
        leased[2].as_deref() == Some("request: Ok(())"),
        "{leased:?}"
    );
    assert!(leased[3].as_ref().unwrap().contains("\"eligible\":true"));

    let mut holder = f.client();
    let mut pushing = holder.transaction().unwrap();
    let row = "SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE";
    pushing.query_one(row, &[]).unwrap();
    let (while_pushed, reading) = answers(&f);
    pushing.commit().unwrap();
    reading.join().unwrap();
    assert_eq!(while_pushed, leased);

    // the lease is still the publisher's, as it was given
    f.store
        .renew_publication(&f.credential, &held, 60_000)
        .unwrap();
    let read = f.store.read_review(&reviewer, &key, 1000).unwrap();
    let inspected = f.store.review_selection(&reviewer, &key).unwrap();
    let inspected = inspected.approval_context(limits()).unwrap();
    let integration = &data.integration;
    let approved = |f: &mut Fixture, work: &PublicationWork| {
        f.store.approve_integration(
            &reviewer,
            work,
            &graph,
            &tasks,
            integration,
            &inspected,
            60_000,
        )
    };
    assert!(
        approved(&mut f, &read).is_err(),
        "nothing is decided through a read"
    );
    assert!(f.store.release_publication(&reviewer, &read).is_err());
    f.store
        .renew_publication(&f.credential, &held, 60_000)
        .unwrap();

    f.store.release_publication(&f.credential, &held).unwrap();
    let review = f
        .store
        .acquire_review(&reviewer, &key, 60_000, 1000)
        .unwrap();
    approved(&mut f, &review).unwrap();
    // the reviewer who decides holds the request, and it is read beside them
    let beside = f.store.read_review(&reviewer, &key, 1000).unwrap();
    let (graph_read, tasks_read) = verified(&beside);
    let now = f
        .store
        .inspect_current_integration(&reviewer, &beside, &graph_read, &tasks_read, integration)
        .unwrap();
    assert!(now.approved && now.eligible);
    f.store.release_publication(&reviewer, &review).unwrap();
    let publisher = f
        .store
        .acquire_publication(&f.credential, &key, 60_000, 1000)
        .unwrap();
    f.store
        .publish_with_tasks(&f.credential, &publisher, &graph, &tasks)
        .unwrap();
    assert!(f.store.read_review(&reviewer, &key, 1000).is_err());
    assert!(
        f.store
            .inspect_current_integration(&reviewer, &beside, &graph_read, &tasks_read, integration)
            .is_err()
    );
}
