//! The portable archive types, with their digests and signing bytes, held to recorded bytes and verdicts.

use alt_json_testkit::fixtures::verify_file;
use alt_portable::http::*;
use alt_portable::*;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

#[test]
fn recorded_bytes_and_verdicts() {
    verify_file::<Blob>(DIR, "Blob");
    verify_file::<Capabilities>(DIR, "Capabilities");
    verify_file::<Catalog>(DIR, "Catalog");
    verify_file::<Envelope>(DIR, "Envelope");
    verify_file::<ExportState>(DIR, "ExportState");
    verify_file::<HistoricalPublication>(DIR, "HistoricalPublication");
    verify_file::<IdentityMapping>(DIR, "IdentityMapping");
    verify_file::<ImportIntent>(DIR, "ImportIntent");
    verify_file::<ImportReceipt>(DIR, "ImportReceipt");
    verify_file::<ImportReceiptBody>(DIR, "ImportReceiptBody");
    verify_file::<ImportState>(DIR, "ImportState");
    verify_file::<InspectedArchive>(DIR, "InspectedArchive");
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
    assert!(
        std::fs::read_to_string(&path).unwrap() == text,
        "digests differ from the recorded ones"
    );
    assert_eq!(lines.len(), 13);
}
