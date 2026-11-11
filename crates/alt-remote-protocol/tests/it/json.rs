//! The protocol types, with their digests and signing bytes, held to recorded bytes and verdicts.

use alt_json_testkit::fixtures::verify_file;
use alt_remote_protocol::authorization::*;
use alt_remote_protocol::change_links::*;
use alt_remote_protocol::delegation::*;
use alt_remote_protocol::login::*;
use alt_remote_protocol::*;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

#[test]
fn recorded_bytes_and_verdicts() {
    verify_file::<Attribution>(DIR, "Attribution");
    verify_file::<Capabilities>(DIR, "Capabilities");
    verify_file::<ChangeEventSummary>(DIR, "ChangeEventSummary");
    verify_file::<ChangeMappingRequest>(DIR, "ChangeMappingRequest");
    verify_file::<ContentEntry>(DIR, "ContentEntry");
    verify_file::<DelegationInfo>(DIR, "DelegationInfo");
    verify_file::<DelegationRequest>(DIR, "DelegationRequest");
    verify_file::<ExportCertificate>(DIR, "ExportCertificate");
    verify_file::<Failure>(DIR, "Failure");
    verify_file::<Grant>(DIR, "Grant");
    verify_file::<Limits>(DIR, "Limits");
    verify_file::<Manifest>(DIR, "Manifest");
    verify_file::<Publication>(DIR, "Publication");
    verify_file::<PublishedBundle>(DIR, "PublishedBundle");
    verify_file::<Receipt>(DIR, "Receipt");
    verify_file::<ReceiptBody>(DIR, "ReceiptBody");
    verify_file::<RepairTaskClaim>(DIR, "RepairTaskClaim");
    verify_file::<RequestKey>(DIR, "RequestKey");
    verify_file::<RequestObservation>(DIR, "RequestObservation");
    verify_file::<RequestState>(DIR, "RequestState");
    verify_file::<SavedCredential>(DIR, "SavedCredential");
    verify_file::<Scope>(DIR, "Scope");
    verify_file::<TransferProof>(DIR, "TransferProof");
}

/// Publications, manifests and receipts built the way the other tests
/// build them, varied field by field.
fn real_samples() -> Vec<(Publication, Manifest, ReceiptBody)> {
    let limits = crate::limits();
    (0..40u64)
        .map(|i| {
            let mut publication = crate::publication();
            publication.key.request_id = format!("request-{i}");
            publication.key.scope.recovery_epoch = 1 + (u64::MAX - 1) / (i + 1);
            if i % 3 == 0 {
                publication.refs.reverse();
                publication.record_roots.reverse();
            }
            if i % 4 == 0 {
                publication.attribution.kind = ActorKind::Agent;
                publication.attribution.on_behalf_of = Some(format!("account-{i}"));
                publication.attribution.delegation_id = Some(format!("delegation-{i}"));
                publication.attribution.agent_session = Some(format!("session-{i}"));
            }
            let mut manifest = Manifest::empty(ObjectFormat::Sha1);
            let digits: Vec<char> = "123456789abcdef".chars().collect();
            manifest.entries = (0..1 + i as usize % 5)
                .map(|n| crate::entry(digits[(i as usize + n * 3) % digits.len()]))
                .collect();
            manifest.entries.dedup_by(|a, b| a.blake3 == b.blake3);
            if i % 2 == 0 {
                manifest.entries.reverse();
            }
            let receipt = ReceiptBody {
                request_digest: publication.digest(limits).unwrap(),
                publication: publication.clone(),
                transaction_sequence: 1 + i * 1_000_003,
                policy_version: 1 + i,
                committed_at_unix_ms: u64::MAX - i,
            };
            (publication, manifest, receipt)
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every digest, id and signing message of the real samples, as the serde
/// encoding produced them.
#[test]
fn recorded_digests_and_signing_bytes() {
    let limits = crate::limits();
    let mut lines = Vec::new();
    for (publication, manifest, receipt) in real_samples() {
        lines.push(format!(
            "publication {} {} {}",
            publication.digest(limits).unwrap(),
            hex(&publication.canonical_bytes(limits).unwrap()),
            hex(&alt_json::to_vec(&publication).unwrap()),
        ));
        if let Ok(id) = manifest.id(limits) {
            lines.push(format!(
                "manifest {id} {} {}",
                hex(&manifest.canonical_bytes(limits).unwrap()),
                hex(&alt_json::to_vec(&manifest).unwrap()),
            ));
        }
        lines.push(format!(
            "receipt {}",
            hex(&receipt.signing_bytes(limits).unwrap())
        ));
    }
    let text = lines.join("\n") + "\n";
    let path = format!("{DIR}/digests.txt");
    assert!(
        std::fs::read_to_string(&path).unwrap() == text,
        "digests differ from the recorded ones"
    );
    println!("alt-remote-protocol digests: {} lines", lines.len());
    assert!(lines.len() > 100);
}
