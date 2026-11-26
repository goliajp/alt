//! Schema 23 indexes what the manifests of committed requests and of archived
//! publications name. These hold the upgrade of an instance that has no such
//! index, and that the question a fetch by digest asks of them is answered by it.
use super::tasks::{task_payload, verified};
use super::*;

/// makes the database the schema 22 one it stands in for; harmless on one that is
pub(super) fn as_schema_22(f: &Fixture) {
    f.client()
        .batch_execute(
            "DELETE FROM alt_schema WHERE version=23;
             DROP INDEX IF EXISTS alt_requests_manifest_entries;
             DROP INDEX IF EXISTS alt_archived_publications_manifest_entries",
        )
        .unwrap();
}

/// the question the native route asks of the manifests, as the server asks it
const NAMED: &str = "WITH q AS MATERIALIZED (SELECT epoch,actor_id,request_id FROM alt_requests WHERE repository=$1 AND state='committed' AND manifest->'entries' @> $2::text::jsonb), p AS MATERIALIZED (SELECT digest FROM alt_archived_publications WHERE repository=$1 AND manifest->'entries' @> $2::text::jsonb) SELECT EXISTS(SELECT 1 FROM q JOIN alt_task_bundles b ON b.repository=$1 AND b.epoch=q.epoch AND b.actor_id=q.actor_id AND b.request_id=q.request_id) OR EXISTS(SELECT 1 FROM p WHERE EXISTS(SELECT 1 FROM alt_archived_records r WHERE r.repository=$1 AND r.digest=p.digest))";

/// what the server asks the manifests for, of a content
fn entry(digest: &str) -> String {
    format!("[{{\"blake3\":\"{digest}\"}}]")
}

/// What the plan of the question reads, of a repository with ten thousand
/// publications of records, for a content none of them names.
fn plan(f: &Fixture) -> String {
    let mut client = f.client();
    client.batch_execute("ANALYZE").unwrap();
    let absent = "0".repeat(64);
    let asked = NAMED
        .replace("$1", "'repository'")
        .replace("$2", &format!("'{}'", entry(&absent)));
    let rows = client
        .query(&format!("EXPLAIN (COSTS OFF) {asked}"), &[])
        .unwrap();
    rows.iter()
        .map(|row| row.get::<_, String>(0) + "\n")
        .collect()
}

/// An instance of schema 22, with publications of records, is upgraded: the
/// two indexes are there, and the question is answered from them where it read
/// every manifest of the repository before.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn an_instance_of_schema_22_is_given_the_indexes_of_what_manifests_name() {
    let mut f = Fixture::new();
    let payload = task_payload("task-publish");
    payload.upload(&mut f.store, &f.credential);
    let work = (f.store)
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let (graph, tasks) = verified(&work, payload.commit);
    (f.store)
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    (f.store)
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    f.client()
        .batch_execute("INSERT INTO alt_requests SELECT (r).* FROM (SELECT jsonb_populate_record(q, jsonb_build_object('request_id','copy-'||n)) AS r FROM alt_requests q, generate_series(1,9999) n WHERE q.state='committed') s; INSERT INTO alt_task_bundles SELECT b.repository,b.bundle_blake3,b.epoch,b.actor_id,'copy-'||n FROM alt_task_bundles b, generate_series(1,9999) n")
        .unwrap();
    let named = payload.manifest.entries[1].blake3.clone();
    let asked = |f: &Fixture, digest: &str| -> bool {
        f.client()
            .query_one(NAMED, &[&"repository", &entry(digest)])
            .unwrap()
            .get(0)
    };
    assert!(asked(&f, &named));

    as_schema_22(&f);
    assert!(f.store.check_connection().is_err());
    let before = plan(&f);
    assert!(!before.contains("manifest_entries"), "{before}");
    super::schema_20::migrate(&mut f, 22).unwrap();
    f.store.check_connection().unwrap();
    let indexes = "SELECT indexname::text FROM pg_indexes WHERE indexname LIKE '%manifest_entries' ORDER BY 1";
    let indexes: Vec<String> = (f.client().query(indexes, &[]).unwrap().iter())
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        indexes,
        [
            "alt_archived_publications_manifest_entries",
            "alt_requests_manifest_entries"
        ]
    );
    let after = plan(&f);
    assert!(after.contains("alt_requests_manifest_entries"), "{after}");
    assert!(asked(&f, &named) && !asked(&f, &"0".repeat(64)));
    assert!(matches!(
        super::schema_20::migrate(&mut f, 22),
        Err(alt_server_store::Error::Precondition(_))
    ));
}
