//! Every JSON value the store keeps in PostgreSQL, held to rows recorded
//! while the serde implementation still wrote them.

use std::collections::BTreeMap;

use alt_json::{FromJson, ToJson};
use alt_json_serde::Serde;
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

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures/stored");

/// The compact text of the value after reading it as its type.
type Reread = fn(Jsonb) -> Vec<u8>;

fn typed<T: ToJson + FromJson>(value: Jsonb) -> Vec<u8> {
    alt_json::to_vec(&value.read::<T>().unwrap()).unwrap()
}

/// For columns that hold free-form events and audit details.
fn untyped(value: Jsonb) -> Vec<u8> {
    alt_json::to_vec(&value.0).unwrap()
}

/// The stored state of a request. A committed one is not the protocol's state: its
/// receipt lacks the publication, which the row has in another column.
fn stored_state(value: Jsonb) -> Vec<u8> {
    if value.0["state"] == "committed" {
        assert!(value.0["receipt"]["body"].get("publication").is_none());
        untyped(value)
    } else {
        typed::<RequestState>(value)
    }
}

/// Every `json` and `jsonb` column of the schema with the type its rows hold.
const COLUMNS: &[(&str, &str, Reread)] = &[
    ("alt_repositories", "policies", typed::<Vec<RefPolicy>>),
    ("alt_credentials", "attribution", typed::<Attribution>),
    ("alt_grants", "grants", typed::<Vec<Grant>>),
    ("alt_requests", "publication", typed::<Publication>),
    ("alt_requests", "manifest", typed::<Manifest>),
    ("alt_requests", "result", stored_state),
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

/// Whether `raw` is exactly what is written for the `T` read from it.
fn document<T: ToJson + FromJson>(raw: &[u8]) -> bool {
    alt_json::from_slice::<T>(raw).is_ok_and(|value| alt_json::to_vec(&value).unwrap() == raw)
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{DIR}/{name}.txt")).unwrap()
}

/// Rows the serde implementation wrote, as PostgreSQL returned them, are
/// read by the current code and written back to the bytes serde wrote.
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
                "SELECT value,value::text FROM stored_json ORDER BY line",
                &[],
            )
            .unwrap();
        for (row, (text, written)) in read.iter().zip(&lines) {
            assert_eq!(row.get::<_, String>(1), *text, "{table}.{column}");
            let reread = reread(row.get(0));
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
