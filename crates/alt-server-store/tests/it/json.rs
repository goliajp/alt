//! The store's own types read and written through alt-json, compared with
//! serde on random values and held to recorded bytes and verdicts.

use alt_json_testkit::fixtures::{check, check_recorded, written_line_as_serde};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::same_as_serde;
use alt_json_testkit::support::Arbitrary;
use alt_remote_protocol::{RecordKind, RecordRoot, RequestKey, ReviewSelection, Scope};
use alt_server_store::*;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

fn any<T: Arbitrary>(rng: &mut Rng) -> T {
    T::arbitrary(rng)
}

#[test]
fn same_as_serde() {
    same_as_serde!(0x5700, 400;
        InstanceSpec, SnapshotPackSegment, SnapshotBlob, SnapshotRepository, SnapshotCatalog,
        ChangeChannel,
    )
    .report("alt-server-store, 6 types in total");
}

#[test]
fn recorded_bytes_and_verdicts() {
    let mut files = 0;
    macro_rules! recorded {
        ($($ty:ident),*) => {$(
            check::<$ty>(DIR, stringify!($ty), 0x5710 + files, 20);
            files += 1;
        )*};
    }
    recorded!(InstanceSpec, SnapshotCatalog, ChangeChannel);
    println!("alt-server-store fixtures: {files} files");
}

fn audit_entry(rng: &mut Rng) -> InstanceAuditEntry {
    InstanceAuditEntry {
        sequence: any(rng),
        actor_id: any(rng),
        action: any(rng),
        subject_id: any(rng),
        occurred_at_unix_ms: any(rng),
    }
}

fn local_user(rng: &mut Rng) -> LocalUser {
    LocalUser {
        actor_id: any(rng),
        login_name: any(rng),
        display_name: any(rng),
        administrator: any(rng),
        disabled: any(rng),
        generation: any(rng),
    }
}

fn scope(rng: &mut Rng) -> Scope {
    Scope {
        installation_id: any(rng),
        repository_instance_id: any(rng),
        recovery_epoch: any(rng),
    }
}

fn selection(rng: &mut Rng) -> ReviewSelection {
    let mut selection: ReviewSelection =
        alt_json::from_str(include_str!("fixtures/review-selection.json")).unwrap();
    selection.publication.key.scope = scope(rng);
    selection.current_refs = any(rng);
    selection.policy_version = any(rng);
    selection
}

/// The types that are only written, as HTTP responses and operator output.
#[test]
fn responses_are_written_as_recorded() {
    let mut rng = Rng::new(0x5720);
    let rng = &mut rng;
    let mut lines = String::new();
    for _ in 0..40 {
        lines += &written_line_as_serde(&InstanceStatus {
            installation_id: any(rng),
            runtime_generation: any(rng),
            schema: any(rng),
            observed_at_unix_ms: any(rng),
            repositories: any(rng),
            stored_bytes: any(rng),
            reserved_bytes: any(rng),
            pending_requests: any(rng),
            recovery_gaps: any(rng),
            pending_events: any(rng),
            last_restore: rng.chance(2).then(|| audit_entry(rng)),
            last_backup: rng.chance(2).then(|| audit_entry(rng)),
            pending_change_links: any(rng),
            recovery_change_links: any(rng),
        });
        lines += &written_line_as_serde(&Page {
            items: (0..rng.below(3))
                .map(|_| WorkbenchRepository {
                    scope: scope(rng),
                    default_branch: any(rng),
                    writes_enabled: any(rng),
                    can_review: any(rng),
                    can_publish: any(rng),
                })
                .collect(),
            next: any(rng),
        });
        lines += &written_line_as_serde(&Page {
            items: (0..rng.below(3))
                .map(|_| WorkbenchRequest {
                    key: RequestKey {
                        scope: scope(rng),
                        actor_id: any(rng),
                        request_id: any(rng),
                    },
                    state: any(rng),
                    digest: any(rng),
                    roots: (0..rng.below(3))
                        .map(|_| RecordRoot {
                            kind: *rng.pick(&[
                                RecordKind::Task,
                                RecordKind::Review,
                                RecordKind::Integration,
                                RecordKind::Delivery,
                            ]),
                            id: any(rng),
                        })
                        .collect(),
                })
                .collect(),
            next: any(rng),
        });
        lines += &written_line_as_serde(&ActiveBrowserSession {
            id: any(rng),
            created_at_unix_ms: any(rng),
            expires_at_unix_ms: any(rng),
            current: any(rng),
        });
        lines += &written_line_as_serde(&BrowserSession {
            user: local_user(rng),
            expires_at_unix_ms: any(rng),
        });
        lines += &written_line_as_serde(&DeviceConsent {
            id: any(rng),
            client_id: "alt-cli",
            scope: "repository",
            state: any(rng),
            expires_at_unix_ms: any(rng),
        });
        lines += &written_line_as_serde(&ClientCredential {
            id: any(rng),
            client_id: any(rng),
            created_at_unix_ms: any(rng),
            expires_at_unix_ms: any(rng),
            current: any(rng),
        });
        lines += &written_line_as_serde(&CurrentReviewAssessment {
            approved: any(rng),
            selection: selection(rng),
            evidence: alt_task_service::EvidenceAssessment {
                evidence: any(rng),
                source_proof: any(rng),
                eligible: any(rng),
                reasons: any(rng),
                collectors: Default::default(),
            },
            eligible: any(rng),
            reason: "current",
        });
    }
    check_recorded(&format!("{DIR}/responses.txt"), &lines);
    println!(
        "alt-server-store responses: {} values",
        lines.lines().count()
    );
}
