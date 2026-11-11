use crate::{Record, Task};
use ed25519_dalek::SigningKey;
use golia_change_link::{
    Body, Counter, Endpoint, Event, IssueVersion, Product, Receipt, RepairRequest, RepairSource,
    SignedEvent,
};

pub(super) fn legacy() -> Task {
    alt_json::from_slice(include_bytes!("fixtures/task-v1.json")).unwrap()
}
fn source(target: &str) -> RepairSource {
    let sender = SigningKey::from_bytes(&[3; 32]);
    let receiver = SigningKey::from_bytes(&[4; 32]);
    let endpoint = |product| Endpoint {
        product,
        installation: format!("{product:?}-installation"),
        workspace: "shared-workspace".into(),
        scope: "remote-project-or-repository".into(),
        generation: Counter(1),
    };
    let request = SignedEvent::sign(
        Event {
            version: 1,
            channel: "paired-integration".into(),
            issuer: endpoint(Product::Sentori),
            receiver: endpoint(Product::Alt),
            entity: "repair-request".into(),
            revision: Counter(1),
            previous_digest: None,
            issued_unix_ms: Counter(100),
            body: Body::RepairRequest(RepairRequest {
                issue: "selected-issue".into(),
                selected_samples: vec!["selected-sample".into()],
                observed_version: IssueVersion {
                    application: None,
                    release: None,
                    release_name: None,
                    environment: None,
                },
                mapping_event_digest: None,
                target_ref: target.into(),
                requested_by: "requester".into(),
            }),
        },
        &sender,
    )
    .unwrap();
    let public = |key: &SigningKey| {
        key.verifying_key()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    RepairSource {
        acceptance: Receipt::accept(&request, Counter(101), &receiver).unwrap(),
        request,
        source_public_key: public(&sender),
        receiver_public_key: public(&receiver),
    }
}

#[test]
fn old_task_encoding_and_identity_remain_byte_compatible() {
    // The canonical identity input was frozen before adding repair_source.
    let expected = alt_hash::blake3::hash(include_bytes!("fixtures/task-v1-identity.bin"))
        .to_hex()
        .to_string();
    let task = legacy();
    assert!(task.spec.repair_source.is_none());
    assert_eq!(task.id().unwrap(), expected);
    let old: alt_json::Value =
        alt_json::from_slice(include_bytes!("fixtures/task-v1.json")).unwrap();
    assert_eq!(alt_json::to_value(&task).unwrap(), old);
}

#[test]
fn source_is_bound_to_the_task_identity_and_its_exact_integration_target() {
    let mut task = legacy();
    let plain = task.id().unwrap();
    task.spec.repair_source = Some(Box::new(source("refs/heads/main")));
    let linked = task.id().unwrap();
    assert_ne!(plain, linked);
    let raw = alt_json::to_vec(&task).unwrap();
    assert_eq!(
        alt_json::from_slice::<Task>(&raw).unwrap().id().unwrap(),
        linked
    );
    // Even a valid original source/receiver signature cannot be reinterpreted
    // as permission to repair a different branch or multiple target branches.
    task.spec.repair_source = Some(Box::new(source("refs/heads/different")));
    assert!(task.validate().is_err());
    task.spec.repair_source = Some(Box::new(source("refs/heads/main")));
    task.target_expected
        .insert("refs/heads/extra".into(), "a".repeat(40));
    assert!(task.validate().is_err());
    task.target_expected.remove("refs/heads/extra");
    task.spec
        .repair_source
        .as_mut()
        .unwrap()
        .acceptance
        .body
        .accepted_unix_ms = Counter(102);
    assert!(task.validate().is_err());
}
