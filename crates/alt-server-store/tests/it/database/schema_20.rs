//! Schema 20 keeps the publication of a committed request once. These hold the
//! upgrade of a schema 19 instance, which stored it in the request's result and in
//! its outbox event as well, to what every read returned before it.
use super::*;
use alt_server_store::Jsonb;

const RECORDED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures/schema19");

/// Makes the database the schema 19 one it stands in for. The longer rows are built
/// from the audit records, which hold whole receipts in every version.
pub(super) fn as_schema_19(f: &Fixture) {
    super::schema_21::as_schema_20(f);
    let mut client = f.client();
    client
        .batch_execute(
            "DELETE FROM alt_schema WHERE version=20;
             ALTER TABLE alt_requests ALTER COLUMN publication SET COMPRESSION default, ALTER COLUMN result SET COMPRESSION default;
             ALTER TABLE alt_audit ALTER COLUMN event SET COMPRESSION default;
             ALTER TABLE alt_outbox ALTER COLUMN event SET COMPRESSION default;
             UPDATE alt_outbox o SET event=a.event FROM alt_audit a WHERE a.repository=o.repository AND a.sequence=o.sequence AND o.event ? 'request'",
        )
        .unwrap();
    let longer = client
        .execute(
            "UPDATE alt_requests q SET result=jsonb_build_object('state','committed','receipt',a.event->'receipt') FROM alt_audit a WHERE q.state='committed' AND NOT q.result #> '{receipt,body}' ? 'publication' AND a.repository=q.repository AND a.event->>'kind'='publication_committed' AND a.event#>'{receipt,body,publication,key}'=q.publication->'key'",
            &[],
        )
        .unwrap();
    let short: i64 = client
        .query_one(
            "SELECT count(*) FROM alt_requests WHERE state='committed' AND NOT result #> '{receipt,body}' ? 'publication'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(short, 0, "{longer} results made longer");
}

pub(super) fn migrate(f: &mut Fixture, from: u32) -> alt_server_store::Result<()> {
    let store = Store::migrate(
        f.client(),
        f.store.bytes().clone(),
        f.store.spec().clone(),
        SigningKey::from_bytes(&f.signing_seed),
        limits(),
        from,
    )?;
    f.spec = store.spec().clone();
    f.store = store;
    f.credential = f.store.authenticate(&f.token).unwrap();
    Ok(())
}

/// schema version, runtime generation, and how many committed results and outbox
/// events hold a publication
fn shape(f: &Fixture) -> (i32, i64, i64, i64) {
    let row = f.client().query_one("SELECT (SELECT max(version) FROM alt_schema),(SELECT runtime_generation FROM alt_instance),(SELECT count(*) FROM alt_requests WHERE result #> '{receipt,body}' ? 'publication'),(SELECT count(*) FROM alt_outbox WHERE event #> '{receipt,body}' ? 'publication')", &[]).unwrap();
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

fn compression(f: &Fixture) -> Vec<(String, String)> {
    f.client()
        .query("SELECT c.relname||'.'||a.attname,a.attcompression::text FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid WHERE (c.relname,a.attname) IN (('alt_requests','publication'),('alt_requests','result'),('alt_audit','event'),('alt_outbox','event')) ORDER BY 1", &[])
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

fn text(f: &Fixture, sql: &str) -> Vec<String> {
    f.client()
        .query(sql, &[])
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// the bytes of what the request query returns for a key
fn observed(f: &mut Fixture, key: &RequestKey) -> Vec<u8> {
    alt_json::to_vec(&f.store.query(&f.credential, key).unwrap()).unwrap()
}

fn values(texts: &[String]) -> Vec<alt_json::Value> {
    texts
        .iter()
        .map(|text| alt_json::parse(text.as_bytes()).unwrap())
        .collect()
}

/// every outbox row, restored to the event it stands for
fn outbox_events(f: &mut Fixture) -> Vec<alt_json::Value> {
    let rows = f
        .client()
        .query(
            "SELECT repository,sequence FROM alt_outbox ORDER BY repository,sequence",
            &[],
        )
        .unwrap();
    rows.iter()
        .map(|row| {
            f.store
                .outbox_event(&row.get::<_, String>(0), row.get::<_, i64>(1) as u64)
                .unwrap()
                .unwrap()
                .0
        })
        .collect()
}

const AUDIT: &str = "SELECT event::text FROM alt_audit ORDER BY repository,sequence";
const OUTBOX: &str = "SELECT event::text FROM alt_outbox ORDER BY repository,sequence";
const RESULTS: &str = "SELECT result::text FROM alt_requests ORDER BY request_id";

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_publication_is_stored_once_and_its_outbox_event_restores_from_the_request() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    let receipt = first.publish_new(&mut f.store, &f.credential);
    let stored: Jsonb = f
        .client()
        .query_one("SELECT result FROM alt_requests", &[])
        .unwrap()
        .get(0);
    let mut whole = alt_json::to_value(&receipt).unwrap();
    whole["body"]
        .as_object_mut()
        .unwrap()
        .remove("publication")
        .unwrap();
    assert_eq!(
        stored.0,
        alt_json::json!({"state":"committed","receipt":whole})
    );
    let event: Jsonb = f
        .client()
        .query_one("SELECT event FROM alt_outbox", &[])
        .unwrap()
        .get(0);
    assert_eq!(
        event.0,
        alt_json::json!({"kind":"publication_committed","request":first.publication.key,"receipt":whole})
    );
    // the audit record is the whole event, and the outbox row restores to it
    let audit: Jsonb = f
        .client()
        .query_one("SELECT event FROM alt_audit", &[])
        .unwrap()
        .get(0);
    assert_eq!(
        audit.0,
        alt_json::json!({"kind":"publication_committed","receipt":receipt})
    );
    assert_eq!(f.store.outbox_event("repository", 1).unwrap(), Some(audit));
    assert_eq!(f.store.outbox_event("repository", 2).unwrap(), None);
    assert_eq!(
        compression(&f),
        [
            "alt_audit.event",
            "alt_outbox.event",
            "alt_requests.publication",
            "alt_requests.result"
        ]
        .map(|column| (column.to_owned(), "l".to_owned()))
    );
    // every reader of the request gets the receipt it was given at commit
    assert_eq!(
        f.store
            .query(&f.credential, &first.publication.key)
            .unwrap(),
        RequestObservation::Recorded {
            key: first.publication.key.clone(),
            result: Box::new(RequestState::Committed {
                receipt: Box::new(receipt.clone())
            })
        }
    );
    assert_eq!(
        f.store
            .begin(&f.credential, &first.publication, &first.manifest, 60_000)
            .unwrap(),
        RequestState::Committed {
            receipt: Box::new(receipt)
        }
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_schema_19_instance_reads_every_request_as_before_the_upgrade() {
    for from in [19, 18] {
        let mut f = Fixture::new();
        let first = Payload::new("first", None);
        let second = Payload::new("second", Some(first.commit));
        let waiting = Payload::new("waiting", Some(second.commit));
        let receipts = [
            first.publish_new(&mut f.store, &f.credential),
            second.publish_new(&mut f.store, &f.credential),
        ];
        waiting.upload(&mut f.store, &f.credential);
        let keys = [&first, &second, &waiting].map(|payload| payload.publication.key.clone());
        let before: Vec<Vec<u8>> = keys.iter().map(|key| observed(&mut f, key)).collect();
        let audit = text(&f, AUDIT);

        as_schema_19(&f);
        if from == 18 {
            f.client()
                .batch_execute("DELETE FROM alt_schema WHERE version=19; ALTER TABLE alt_pins DROP CONSTRAINT alt_pins_no_ref")
                .unwrap();
        }
        assert_eq!(shape(&f), (from as i32, 1, 2, 2));
        // as that version wrote them: the outbox event is the audit record
        assert_eq!(text(&f, OUTBOX), audit);
        assert!(f.store.check_connection().is_err());

        migrate(&mut f, from).unwrap();
        assert_eq!(shape(&f), (21, 2, 0, 0));
        assert!(compression(&f).iter().all(|(_, method)| method == "l"));
        let after: Vec<Vec<u8>> = keys.iter().map(|key| observed(&mut f, key)).collect();
        assert_eq!(after, before, "from schema {from}");
        assert_eq!(text(&f, AUDIT), audit);
        assert_eq!(outbox_events(&mut f), values(&audit));
        for (payload, receipt) in [&first, &second].into_iter().zip(&receipts) {
            assert_eq!(
                f.store
                    .begin(
                        &f.credential,
                        &payload.publication,
                        &payload.manifest,
                        60_000
                    )
                    .unwrap(),
                RequestState::Committed {
                    receipt: Box::new(receipt.clone())
                }
            );
        }
        // the upgraded instance goes on publishing
        let third = Payload::new("third", Some(second.commit));
        third.publish_new(&mut f.store, &f.credential);
        assert_eq!(shape(&f), (21, 2, 0, 0));
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_row_that_would_not_read_back_fails_the_upgrade_and_leaves_schema_19() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.publish_new(&mut f.store, &f.credential);
    Payload::new("second", Some(first.commit)).publish_new(&mut f.store, &f.credential);
    let before = observed(&mut f, &first.publication.key);
    as_schema_19(&f);
    let kept = (text(&f, RESULTS), text(&f, OUTBOX));

    let refused = |f: &mut Fixture, damage: &str, repair: &str, what: &'static str| {
        f.client().batch_execute(damage).unwrap();
        assert!(
            matches!(migrate(f, 19), Err(alt_server_store::Error::Integrity(found)) if found == what),
            "{what}"
        );
        f.client().batch_execute(repair).unwrap();
        // nothing of the upgrade stays: version, generation, rows, column settings
        assert_eq!(shape(f), (19, 1, 2, 2));
        assert_eq!((text(f, RESULTS), text(f, OUTBOX)), kept);
        assert!(compression(f).iter().all(|(_, method)| method.is_empty()));
    };
    refused(
        &mut f,
        "UPDATE alt_requests SET publication=jsonb_set(publication,'{content_manifest}','\"other\"') WHERE request_id='second'",
        "UPDATE alt_requests SET publication=result#>'{receipt,body,publication}' WHERE request_id='second'",
        "committed request differs from its publication",
    );
    refused(
        &mut f,
        "UPDATE alt_outbox SET event=jsonb_set(event,'{receipt,body,publication,key,request_id}','\"elsewhere\"') WHERE sequence=2",
        "UPDATE alt_outbox o SET event=a.event FROM alt_audit a WHERE a.repository=o.repository AND a.sequence=o.sequence",
        "outbox event differs from its request",
    );

    // a row already in the new form is left as it is
    f.client()
        .batch_execute(
            "UPDATE alt_requests SET result=result #- '{receipt,body,publication}' WHERE request_id='first';
             UPDATE alt_outbox SET event=jsonb_build_object('kind',event->'kind','request',event#>'{receipt,body,publication,key}','receipt',(event->'receipt') #- '{body,publication}') WHERE sequence=1",
        )
        .unwrap();
    migrate(&mut f, 19).unwrap();
    assert_eq!(shape(&f), (21, 2, 0, 0));
    assert_eq!(observed(&mut f, &first.publication.key), before);
    assert_eq!(outbox_events(&mut f), values(&text(&f, AUDIT)));
}

fn recorded(name: &str) -> Vec<(String, String)> {
    std::fs::read_to_string(format!("{RECORDED}/{name}.txt"))
        .unwrap()
        .lines()
        .map(|line| {
            let (stored, written) = line.split_once('\t').unwrap();
            (stored.to_owned(), written.to_owned())
        })
        .collect()
}

/// Puts one recorded row into a repository of its own, so that recorded keys may
/// repeat: a request with the result, and the audit record and outbox row of the event.
/// The request of an event is the one its receipt names.
fn place(client: &mut Client, name: &str, request: Option<(&str, &str)>, event: Option<&str>) {
    client
        .execute(
            "INSERT INTO alt_repositories(id,recovery_epoch,object_format,quota_bytes) VALUES($1,1,'sha1',0)",
            &[&name],
        )
        .unwrap();
    match (request, event) {
        (Some((state, result)), Some(event)) => {
            client.execute("INSERT INTO alt_requests(repository,epoch,actor_id,request_id,digest,publication,manifest,state,result,expires_at,reserved_bytes) SELECT $1,(e#>>'{receipt,body,publication,key,scope,recovery_epoch}')::bigint,e#>>'{receipt,body,publication,key,actor_id}',e#>>'{receipt,body,publication,key,request_id}',e#>>'{receipt,body,request_digest}',e#>'{receipt,body,publication}','{}',$2,$3::text::jsonb,0,0 FROM (SELECT $4::text::jsonb AS e)", &[&name, &state, &result, &event]).unwrap();
        }
        (Some((state, result)), None) => {
            client.execute("INSERT INTO alt_requests(repository,epoch,actor_id,request_id,digest,publication,manifest,state,result,expires_at,reserved_bytes) VALUES($1,1,'owner','recorded','','{}','{}',$2,$3::text::jsonb,0,0)", &[&name, &state, &result]).unwrap();
        }
        (None, _) => {}
    }
    if let Some(event) = event {
        client.execute("INSERT INTO alt_audit(repository,sequence,actor_id,event) VALUES($1,1,'owner',$2::text::jsonb)", &[&name, &event]).unwrap();
        client
            .execute(
                "INSERT INTO alt_outbox(repository,sequence,event) VALUES($1,1,$2::text::jsonb)",
                &[&name, &event],
            )
            .unwrap();
    }
}

/// Rows a schema 19 instance held, recorded as PostgreSQL returned them with the
/// bytes they were read as. After the upgrade the same bytes are read: results
/// through the request query where this instance may ask for the request, and
/// through the restored outbox event everywhere.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn rows_recorded_at_schema_19_are_read_unchanged_after_the_upgrade() {
    let mut f = Fixture::new();
    as_schema_19(&f);
    let mut client = f.client();
    let results = recorded("alt_requests.result");
    let events = recorded("alt_outbox.event");
    let mut committed = 0;
    for (line, (stored, _)) in results.iter().enumerate() {
        let value = alt_json::parse(stored.as_bytes()).unwrap();
        let state = value["state"].as_str().unwrap();
        if state == "committed" {
            committed += 1;
            let event = alt_json::to_vec(
                &alt_json::json!({"kind":"publication_committed","receipt":value["receipt"]}),
            )
            .unwrap();
            place(
                &mut client,
                &format!("result-{line}"),
                Some((state, stored)),
                Some(std::str::from_utf8(&event).unwrap()),
            );
        } else {
            place(
                &mut client,
                &format!("result-{line}"),
                Some((state, stored)),
                None,
            );
        }
    }
    let mut publications = 0;
    for (line, (stored, _)) in events.iter().enumerate() {
        let value = alt_json::parse(stored.as_bytes()).unwrap();
        let name = format!("event-{line}");
        if value["kind"] == "publication_committed" {
            publications += 1;
            let result = alt_json::to_vec(
                &alt_json::json!({"state":"committed","receipt":value["receipt"]}),
            )
            .unwrap();
            place(
                &mut client,
                &name,
                Some(("committed", std::str::from_utf8(&result).unwrap())),
                Some(stored),
            );
        } else {
            place(&mut client, &name, None, Some(stored));
        }
    }
    // an event of another kind holds no publication and is not a row to rewrite
    let other = r#"{"kind": "portable_import_committed", "receipt": {"version": 1}}"#;
    place(&mut client, "other", None, Some(other));
    assert!(committed > 0 && publications > 0);
    assert_eq!(shape(&f).2, (committed + publications) as i64);

    migrate(&mut f, 19).unwrap();
    assert_eq!(shape(&f), (21, 2, 0, 0));

    let mut queried = 0;
    for (line, (stored, written)) in results.iter().enumerate() {
        let name = format!("result-{line}");
        let value = alt_json::parse(stored.as_bytes()).unwrap();
        if value["state"] != "committed" {
            let row = client
                .query_one(
                    "SELECT result::text,result FROM alt_requests WHERE repository=$1",
                    &[&name],
                )
                .unwrap();
            assert_eq!(&row.get::<_, String>(0), stored, "{name}");
            let state: RequestState = row.get::<_, Jsonb>(1).read().unwrap();
            assert_eq!(
                alt_json::to_vec(&state).unwrap(),
                written.as_bytes(),
                "{name}"
            );
            continue;
        }
        let event = f.store.outbox_event(&name, 1).unwrap().unwrap();
        let receipt: Receipt = alt_json::from_value(event.0["receipt"].clone()).unwrap();
        let state = RequestState::Committed {
            receipt: Box::new(receipt.clone()),
        };
        assert_eq!(
            alt_json::to_vec(&state).unwrap(),
            written.as_bytes(),
            "{name}"
        );
        // the same row under the repository its key names, where the request query finds it
        let key = &receipt.body.publication.key;
        if key.scope.installation_id == "installation" && key.actor_id == "owner" {
            client
                .batch_execute("DELETE FROM alt_requests WHERE repository='repository'")
                .unwrap();
            client.execute("INSERT INTO alt_requests SELECT 'repository',epoch,actor_id,request_id,digest,publication,manifest,state,result,expires_at,reserved_bytes FROM alt_requests WHERE repository=$1", &[&name]).unwrap();
            if let Ok(RequestObservation::Recorded { result, .. }) =
                f.store.query(&f.credential, key)
            {
                assert_eq!(
                    alt_json::to_vec(&result).unwrap(),
                    written.as_bytes(),
                    "{name}"
                );
                queried += 1;
            }
        }
    }
    for (line, (stored, written)) in events.iter().enumerate() {
        let name = format!("event-{line}");
        let event = f.store.outbox_event(&name, 1).unwrap().unwrap();
        assert_eq!(
            alt_json::to_vec(&event.0).unwrap(),
            written.as_bytes(),
            "{name}"
        );
        // the audit record is as it was recorded
        let audit: String = client
            .query_one(
                "SELECT event::text FROM alt_audit WHERE repository=$1",
                &[&name],
            )
            .unwrap()
            .get(0);
        assert_eq!(&audit, stored, "{name}");
    }
    assert_eq!(
        client
            .query_one(
                "SELECT event::text FROM alt_outbox WHERE repository='other'",
                &[]
            )
            .unwrap()
            .get::<_, String>(0),
        other
    );
    assert_eq!(
        f.store.outbox_event("other", 1).unwrap().unwrap().0,
        alt_json::parse(other.as_bytes()).unwrap()
    );
    assert!(queried > 0);
    println!(
        "schema 19 rows: {} results ({committed} committed, {queried} read through the request query), {} outbox events ({publications} publications)",
        results.len(),
        events.len()
    );
}
