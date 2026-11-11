//! Every JSON value the store keeps in PostgreSQL, read through serde and
//! through alt-json and held to rows recorded from the serde
//! implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use alt_json_serde::Serde;
use alt_json_testkit::check::same;
use alt_json_testkit::support::Comparable;
use alt_portable::{Envelope, HistoricalPublication, ImportIntent, ImportReceipt};
use alt_remote_protocol::delegation::AgentScope;
use alt_remote_protocol::{
    Attribution, ContentEntry, Manifest, Publication, Receipt, RequestState, TransferProof,
};
use alt_repository_service::{Grant, RefPolicy};
use alt_server_store::{ChangeChannel, Jsonb};
use alt_task_service::CollectorPolicy;
use golia_change_link::HistoricalEvent;
use golia_platform_contract::{AccountAuthorization, AltServiceAuthorization, Audience};
use postgres::Client;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures/stored");

/// The compact text both libraries write after reading the value as `T`,
/// or `None` when both refuse it.
type Reread = fn(&serde_json::Value, &alt_json::Value) -> Option<Vec<u8>>;

fn typed<T: Comparable>(theirs: &serde_json::Value, ours: &alt_json::Value) -> Option<Vec<u8>> {
    let theirs = serde_json::from_value::<T>(theirs.clone());
    let ours = alt_json::from_value::<T>(ours.clone());
    match (ours, theirs) {
        (Ok(ours), Ok(theirs)) => {
            let ours = alt_json::to_vec(&ours).unwrap();
            assert_eq!(ours, serde_json::to_vec(&theirs).unwrap());
            Some(ours)
        }
        (Err(_), Err(_)) => None,
        (ours, theirs) => panic!(
            "verdict differs for {}: {:?} vs {:?}",
            std::any::type_name::<T>(),
            ours.map(drop),
            theirs.map(drop)
        ),
    }
}

/// For columns that hold free-form events and audit details.
fn untyped(_: &serde_json::Value, ours: &alt_json::Value) -> Option<Vec<u8>> {
    Some(alt_json::to_vec(ours).unwrap())
}

/// Every `json` and `jsonb` column of the schema with the type its rows hold.
const COLUMNS: &[(&str, &str, Reread)] = &[
    ("alt_repositories", "policies", typed::<Vec<RefPolicy>>),
    ("alt_credentials", "attribution", typed::<Attribution>),
    ("alt_grants", "grants", typed::<Vec<Grant>>),
    ("alt_requests", "publication", typed::<Publication>),
    ("alt_requests", "manifest", typed::<Manifest>),
    ("alt_requests", "result", typed::<RequestState>),
    ("alt_audit", "event", untyped),
    ("alt_outbox", "event", untyped),
    ("alt_collector_policies", "policy", typed::<CollectorPolicy>),
    ("alt_instance_audit", "details", untyped),
    ("alt_cloud_binding", "audience", typed::<Serde<Audience>>),
    (
        "alt_cloud_binding",
        "service",
        typed::<Serde<AltServiceAuthorization>>,
    ),
    (
        "alt_cloud_credentials",
        "claims",
        typed::<Serde<AccountAuthorization>>,
    ),
    ("alt_delegations", "scope", typed::<AgentScope>),
    (
        "alt_publication_provenance",
        "proofs",
        typed::<Vec<TransferProof>>,
    ),
    ("alt_portable_exports", "envelope", typed::<Envelope>),
    (
        "alt_portable_exports",
        "documents",
        typed::<BTreeMap<String, Vec<u8>>>,
    ),
    ("alt_portable_imports", "intent", typed::<ImportIntent>),
    ("alt_portable_imports", "envelope", typed::<Envelope>),
    ("alt_portable_imports", "receipt", typed::<ImportReceipt>),
    (
        "alt_archived_publications",
        "publication",
        typed::<Publication>,
    ),
    ("alt_archived_publications", "manifest", typed::<Manifest>),
    ("alt_archived_publications", "receipt", typed::<Receipt>),
    (
        "alt_archived_publications",
        "original",
        typed::<HistoricalPublication>,
    ),
    ("alt_link_channels", "specification", typed::<ChangeChannel>),
    (
        "alt_request_content",
        "selected_entries",
        typed::<Vec<ContentEntry>>,
    ),
    ("alt_segments", "selection", typed::<BTreeMap<String, u64>>),
];

/// Whether `raw` is exactly what the libraries write for the `T` they read
/// from it; both must agree.
fn document<T: Comparable>(raw: &[u8]) -> bool {
    let theirs = serde_json::from_slice::<T>(raw)
        .is_ok_and(|value| serde_json::to_vec(&value).unwrap() == raw);
    let ours =
        alt_json::from_slice::<T>(raw).is_ok_and(|value| alt_json::to_vec(&value).unwrap() == raw);
    assert_eq!(ours, theirs, "{}", std::any::type_name::<T>());
    ours
}

static RECORDED: Mutex<BTreeMap<String, BTreeSet<String>>> = Mutex::new(BTreeMap::new());

/// Keeps a bounded, arbitrary but repeatable choice of the lines seen.
fn record(name: &str, line: String) {
    if std::env::var_os("ALT_JSON_RECORD").is_none() {
        return;
    }
    let mut recorded = RECORDED.lock().unwrap();
    let lines = recorded.entry(name.to_owned()).or_default();
    lines.insert(line);
    let mut chosen: Vec<&String> = lines.iter().collect();
    chosen.sort_by_key(|line| *alt_hash::blake3::hash(line.as_bytes()).as_bytes());
    let mut budget = 48 * 1024;
    let mut text = String::new();
    for line in chosen.into_iter().take(24) {
        if !text.is_empty() && line.len() > budget {
            continue;
        }
        budget = budget.saturating_sub(line.len());
        text += line;
        text.push('\n');
    }
    std::fs::create_dir_all(DIR).unwrap();
    std::fs::write(format!("{DIR}/{name}.txt"), text).unwrap();
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Compares the two libraries on every JSON value in the database. Called
/// when a test's database is dropped, so each test contributes the rows it
/// really wrote.
pub(crate) fn compare_stored(client: &mut Client) {
    let listed = client
        .query(
            "SELECT table_name::text,column_name::text FROM information_schema.columns WHERE table_schema='public' AND data_type IN ('json','jsonb')",
            &[],
        )
        .unwrap();
    for row in &listed {
        let (table, column): (String, String) = (row.get(0), row.get(1));
        let reread = COLUMNS
            .iter()
            .find(|(t, c, _)| (*t, *c) == (table.as_str(), column.as_str()))
            .unwrap_or_else(|| panic!("{table}.{column} is not listed"))
            .2;
        let rows = client
            .query(
                &format!(
                    "SELECT {column},{column},{column}::text FROM {table} WHERE {column} IS NOT NULL"
                ),
                &[],
            )
            .unwrap();
        for row in rows {
            let theirs: serde_json::Value = row.get(0);
            let Jsonb(ours) = row.get(1);
            assert!(same(&ours, &theirs), "{table}.{column}");
            assert_eq!(
                alt_json::to_vec(&ours).unwrap(),
                serde_json::to_vec(&theirs).unwrap()
            );
            if let Some(written) = reread(&theirs, &ours) {
                let text: String = row.get(2);
                record(
                    &format!("{table}.{column}"),
                    format!("{text}\t{}", String::from_utf8(written).unwrap()),
                );
            }
        }
    }
    let has_history: bool = client
        .query_one("SELECT to_regclass('alt_link_history') IS NOT NULL", &[])
        .unwrap()
        .get(0);
    if has_history {
        for row in client
            .query("SELECT history FROM alt_link_history", &[])
            .unwrap()
        {
            let raw: Vec<u8> = row.get(0);
            // tests also store history that the store must refuse
            if document::<Serde<HistoricalEvent>>(&raw) {
                record("alt_link_history.history", hex(&raw));
            }
        }
    }
    // the documents of an export are the bytes its digests were taken over
    let has_exports: bool = client
        .query_one(
            "SELECT to_regclass('alt_portable_exports') IS NOT NULL",
            &[],
        )
        .unwrap()
        .get(0);
    if has_exports {
        for row in client
            .query("SELECT documents FROM alt_portable_exports", &[])
            .unwrap()
        {
            let Jsonb(documents) = row.get(0);
            let documents: BTreeMap<String, Vec<u8>> = alt_json::from_value(documents).unwrap();
            for raw in documents.values() {
                let kind = if document::<Publication>(raw) {
                    "Publication"
                } else if document::<Manifest>(raw) {
                    "Manifest"
                } else if document::<Receipt>(raw) {
                    "Receipt"
                } else if document::<ImportReceipt>(raw) {
                    "ImportReceipt"
                } else {
                    panic!("export document of an unknown type")
                };
                record(
                    &format!("document.{kind}"),
                    String::from_utf8(raw.clone()).unwrap(),
                );
            }
        }
    }
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{DIR}/{name}.txt")).unwrap()
}

/// Rows the serde implementation wrote, as PostgreSQL returned them, are
/// read by the current code and written back to the recorded bytes.
#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn rows_written_through_serde_are_read_unchanged() {
    let f = super::Fixture::new();
    let mut client = f.client();
    client
        .batch_execute("CREATE TEMP TABLE stored_json(line int PRIMARY KEY, value jsonb NOT NULL)")
        .unwrap();
    let mut rows = 0;
    for (table, column, reread) in COLUMNS {
        let stored = fixture(&format!("{table}.{column}"));
        client.batch_execute("TRUNCATE stored_json").unwrap();
        let lines: Vec<(&str, &str)> = stored
            .lines()
            .map(|line| line.split_once('\t').unwrap())
            .collect();
        assert!(!lines.is_empty(), "{table}.{column}: nothing recorded");
        for (number, (text, _)) in lines.iter().enumerate() {
            client
                .execute(
                    "INSERT INTO stored_json VALUES($1,$2::text::jsonb)",
                    &[&(number as i32), text],
                )
                .unwrap();
        }
        let read = client
            .query(
                "SELECT value,value,value::text FROM stored_json ORDER BY line",
                &[],
            )
            .unwrap();
        for (row, (text, written)) in read.iter().zip(&lines) {
            assert_eq!(row.get::<_, String>(2), *text, "{table}.{column}");
            let Jsonb(ours) = row.get(1);
            let reread = reread(&row.get(0), &ours).unwrap();
            assert_eq!(
                String::from_utf8(reread).unwrap(),
                *written,
                "{table}.{column}"
            );
            rows += 1;
        }
    }
    println!("stored json: {} columns, {rows} rows", COLUMNS.len());
}

/// Signed change-link history and the documents of portable exports, whose
/// bytes are compared with a fresh encoding or carry a digest.
#[test]
fn recorded_documents_are_written_back_unchanged() {
    let mut documents = 0;
    for line in fixture("alt_link_history.history").lines() {
        let raw: Vec<u8> = line
            .as_bytes()
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        assert!(document::<Serde<HistoricalEvent>>(&raw));
        documents += 1;
    }
    let mut check = |name: &str, known: fn(&[u8]) -> bool| {
        for line in fixture(name).lines() {
            assert!(known(line.as_bytes()), "{name}");
            documents += 1;
        }
    };
    check("document.Publication", document::<Publication>);
    check("document.Manifest", document::<Manifest>);
    check("document.Receipt", document::<Receipt>);
    check("document.ImportReceipt", document::<ImportReceipt>);
    println!("stored json: {documents} documents");
}
