//! The protocol types read and written through alt-json, compared with
//! serde on random values and held to recorded bytes, verdicts, digests
//! and signing bytes.

use alt_json_testkit::arbitrary::from_serde;
use alt_json_testkit::drive::exercise_with;
use alt_json_testkit::fixtures::{check, check_recorded, record, verify_file};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::same_as_serde;
use alt_json_testkit::support::Arbitrary;
use alt_remote_protocol::authorization::*;
use alt_remote_protocol::change_links::*;
use alt_remote_protocol::delegation::*;
use alt_remote_protocol::login::*;
use alt_remote_protocol::*;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

fn any<T: Arbitrary>(rng: &mut Rng) -> T {
    T::arbitrary(rng)
}

// enums with a tag cannot be generated from their serde implementation

fn request_state(rng: &mut Rng) -> RequestState {
    match rng.below(5) {
        0 => RequestState::Receiving {
            digest: any(rng),
            expires_at_unix_ms: any(rng),
        },
        1 => RequestState::Ready { digest: any(rng) },
        2 => RequestState::Committed {
            receipt: Box::new(from_serde(rng)),
        },
        3 => RequestState::Rejected {
            digest: any(rng),
            failure: from_serde(rng),
        },
        _ => RequestState::Expired { digest: any(rng) },
    }
}

fn request_observation(rng: &mut Rng) -> RequestObservation {
    match rng.below(3) {
        0 => RequestObservation::Recorded {
            key: from_serde(rng),
            result: Box::new(request_state(rng)),
        },
        1 => RequestObservation::Unknown {
            key: from_serde(rng),
        },
        _ => RequestObservation::NotSeen {
            key: from_serde(rng),
        },
    }
}

fn transfer_reason(rng: &mut Rng) -> TransferReason {
    match rng.below(2) {
        0 => TransferReason::Migration {
            export: Box::new(from_serde(rng)),
        },
        _ => TransferReason::Recovery {
            backup_digest: any(rng),
        },
    }
}

fn transfer_body(rng: &mut Rng) -> TransferBody {
    TransferBody {
        version: any(rng),
        publication_digest: any(rng),
        source: from_serde(rng),
        target: from_serde(rng),
        source_public_key: any(rng),
        target_public_key: any(rng),
        operation_id: any(rng),
        previous_proof: any(rng),
        reason: transfer_reason(rng),
    }
}

fn transfer_proof(rng: &mut Rng) -> TransferProof {
    TransferProof {
        body: transfer_body(rng),
        signature: any(rng),
    }
}

fn published_bundle(rng: &mut Rng) -> PublishedBundle {
    PublishedBundle {
        publication: from_serde(rng),
        manifest: from_serde(rng),
        receipt: from_serde(rng),
        provenance: (0..rng.below(3)).map(|_| transfer_proof(rng)).collect(),
    }
}

#[test]
fn same_as_serde() {
    let mut total = same_as_serde!(0x9000, 400;
        Grant, RefSelector, ChangeMappingRequest, ChangeWithdrawalRequest, RepairBindingRequest,
        ChangeChannelStatus, ChangeEventSummary, ChangeEventPage, RepairTaskClaim, AgentScope,
        DelegationRequest, DelegationInfo, DelegationPage, CapabilityResponse, BeginRequest,
        ApprovalRequest, ApprovalContext, UploadStatus, PrincipalInfo, RepositoryInfo,
        ReviewSelection, ObjectFormat, Scope, RequestKey, Limits, ActorKind, Attribution, DeploymentMode,
        Feature, Capabilities, DeviceAuthorizationRequest, DeviceAuthorizationResponse, DeviceTokenRequest,
        DeviceTokenResponse, DeviceError, DeviceFailure, SavedCredential, ExportCertificateBody,
        ExportCertificate, RefUpdate, RecordKind, RecordRoot, Publication, ContentKind, Chunk, ContentEntry,
        Manifest, ErrorCode, Retry, Failure, ReceiptBody, Receipt,
    );
    let mut by_hand = |name: &str, tally: alt_json_testkit::support::Tally| {
        tally.report(name);
        total.add(&tally);
    };
    by_hand("RequestState", exercise_with(0x9101, 800, request_state));
    by_hand(
        "RequestObservation",
        exercise_with(0x9102, 800, request_observation),
    );
    by_hand(
        "TransferReason",
        exercise_with(0x9103, 800, transfer_reason),
    );
    by_hand("TransferBody", exercise_with(0x9104, 800, transfer_body));
    by_hand("TransferProof", exercise_with(0x9105, 800, transfer_proof));
    by_hand(
        "PublishedBundle",
        exercise_with(0x9106, 400, published_bundle),
    );
    total.report("alt-remote-protocol, 58 types in total");
}

#[test]
fn recorded_bytes_and_verdicts() {
    let mut files = 0;
    macro_rules! recorded {
        ($($ty:ident),*) => {$(
            check::<$ty>(DIR, stringify!($ty), 0x7700 + files, 25);
            files += 1;
        )*};
    }
    recorded!(
        Publication,
        Manifest,
        ContentEntry,
        ReceiptBody,
        Receipt,
        Failure,
        Capabilities,
        Limits,
        Scope,
        RequestKey,
        Attribution,
        ExportCertificate,
        Grant,
        DelegationRequest,
        DelegationInfo,
        SavedCredential,
        ChangeMappingRequest,
        ChangeEventSummary,
        RepairTaskClaim
    );
    let by_hand = |name: &str, text: String| check_recorded(&format!("{DIR}/{name}.txt"), &text);
    by_hand("RequestState", record(0x7801, 25, request_state));
    by_hand(
        "RequestObservation",
        record(0x7802, 25, request_observation),
    );
    by_hand("TransferProof", record(0x7803, 25, transfer_proof));
    by_hand("PublishedBundle", record(0x7804, 15, published_bundle));
    verify_file::<PublishedBundle>(DIR, "PublishedBundle");
    verify_file::<RequestState>(DIR, "RequestState");
    verify_file::<RequestObservation>(DIR, "RequestObservation");
    verify_file::<TransferProof>(DIR, "TransferProof");
    println!("alt-remote-protocol fixtures: {} files", files + 4);
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
    if std::env::var_os("ALT_JSON_RECORD").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    assert!(
        std::fs::read_to_string(&path).unwrap() == text,
        "digests differ from the recorded ones"
    );
    println!("alt-remote-protocol digests: {} lines", lines.len());
    assert!(lines.len() > 100);
}
