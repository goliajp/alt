//! The portable archive types read and written through alt-json, compared
//! with serde on random values and held to recorded bytes, verdicts,
//! digests and signing bytes.

use alt_json_testkit::arbitrary::from_serde;
use alt_json_testkit::drive::exercise_with;
use alt_json_testkit::fixtures::{check, check_recorded, record, verify_file};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::same_as_serde;
use alt_json_testkit::support::Arbitrary;
use alt_portable::http::*;
use alt_portable::*;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

fn any<T: Arbitrary>(rng: &mut Rng) -> T {
    T::arbitrary(rng)
}

// enums with a tag cannot be generated from their serde implementation

fn import_state(rng: &mut Rng) -> ImportState {
    match rng.below(5) {
        0 => ImportState::Receiving {
            digest: any(rng),
            expires_at_unix_ms: any(rng),
            missing: (0..rng.below(3)).map(|_| from_serde(rng)).collect(),
        },
        1 => ImportState::Committed {
            receipt: Box::new(from_serde(rng)),
        },
        2 => ImportState::Abandoned { digest: any(rng) },
        3 => ImportState::Unknown {
            key: from_serde(rng),
        },
        _ => ImportState::NotSeen {
            key: from_serde(rng),
        },
    }
}

fn export_state(rng: &mut Rng) -> ExportState {
    match rng.below(5) {
        0 => ExportState::Preparing {
            scope: from_serde(rng),
            export_id: any(rng),
            catalog_digest: any(rng),
            snapshot_sequence: any(rng),
        },
        1 => ExportState::Available {
            envelope: Box::new(from_serde(rng)),
            frozen: any(rng),
        },
        2 => ExportState::Retired {
            scope: from_serde(rng),
            export_id: any(rng),
            catalog_digest: any(rng),
        },
        3 => ExportState::Unknown {
            scope: from_serde(rng),
            export_id: any(rng),
        },
        _ => ExportState::NotSeen {
            scope: from_serde(rng),
            export_id: any(rng),
        },
    }
}

#[test]
fn same_as_serde() {
    let mut total = same_as_serde!(0xb0a0, 300;
        Blob, Object, Reference, HistoricalPublication, IdentityRequirement, Catalog,
        Envelope, IdentityMapping, ImportIntent, ImportReceiptBody, ImportReceipt, BeginImport, Bounds,
        Capabilities, InspectArchive, InspectedArchive,
    );
    for (name, tally) in [
        ("ImportState", exercise_with(0xb0a1, 600, import_state)),
        ("ExportState", exercise_with(0xb0a2, 600, export_state)),
    ] {
        tally.report(name);
        total.add(&tally);
    }
    total.report("alt-portable, 18 types in total");
}

#[test]
fn recorded_bytes_and_verdicts() {
    let mut files = 0;
    macro_rules! recorded {
        ($($ty:ident),*) => {$(
            check::<$ty>(DIR, stringify!($ty), 0xb0b0 + files, 15);
            files += 1;
        )*};
    }
    recorded!(
        Catalog,
        Envelope,
        IdentityMapping,
        ImportIntent,
        ImportReceiptBody,
        ImportReceipt,
        Blob,
        HistoricalPublication,
        Capabilities,
        InspectedArchive
    );
    check_recorded(
        &format!("{DIR}/ImportState.txt"),
        &record(0xb0c1, 15, import_state),
    );
    check_recorded(
        &format!("{DIR}/ExportState.txt"),
        &record(0xb0c2, 15, export_state),
    );
    verify_file::<ImportState>(DIR, "ImportState");
    verify_file::<ExportState>(DIR, "ExportState");
    println!("alt-portable fixtures: {} files", files + 2);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The catalog, envelope, import intent and import receipt the other tests
/// build, with every digest and signed message, as the serde encoding
/// produced them. Signatures are deterministic, so the signed envelope and
/// receipt are fixed bytes too.
#[test]
fn recorded_digests_and_signing_bytes() {
    let limits = crate::limits();
    let (catalog, _) = crate::fixture();
    let (envelope, _, archive) = crate::packed();
    let intent = crate::import_intent(&envelope);
    let target = alt_crypto::ed25519::SigningKey::from_bytes(&[8; 32]);
    let mut lines = vec![
        format!("catalog-digest {}", catalog.digest(limits).unwrap()),
        format!(
            "catalog-canonical {}",
            hex(&catalog.canonical_bytes(limits).unwrap())
        ),
        format!("catalog {}", hex(&alt_json::to_vec(&catalog).unwrap())),
        format!("envelope {}", hex(&alt_json::to_vec(&envelope).unwrap())),
        format!(
            "archive-blake3 {}",
            alt_hash::blake3::hash(&archive).to_hex()
        ),
        format!("intent-digest {}", intent.digest(limits).unwrap()),
        format!("intent {}", hex(&alt_json::to_vec(&intent).unwrap())),
    ];
    for sequence in [1, 1 << 53, u64::MAX] {
        let body = ImportReceiptBody {
            version: 1,
            intent: intent.clone(),
            transaction_sequence: sequence,
            committed_at_unix_ms: u64::MAX - sequence / 2,
        };
        lines.push(format!(
            "receipt-signing {}",
            hex(&body.signing_bytes(limits).unwrap())
        ));
        let receipt = ImportReceipt::sign(body, &target, limits).unwrap();
        lines.push(format!(
            "receipt {}",
            hex(&alt_json::to_vec(&receipt).unwrap())
        ));
    }
    let text = lines.join("\n") + "\n";
    let path = format!("{DIR}/digests.txt");
    if std::env::var_os("ALT_JSON_RECORD").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    assert!(
        std::fs::read_to_string(&path).unwrap() == text,
        "digests differ from the recorded ones"
    );
    assert_eq!(lines.len(), 13);
}
