//! The vendored change-link types that alt embeds in its own records,
//! driven through the bridge and compared with serde_json.

use alt_json::{FromJson, ToJson};
use alt_json_testkit::drive::{exercise_bridged, exercise_with};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::support::{Arbitrary, Tally};
use golia_change_link::{
    Body, Counter, Decision, Delivery, Endpoint, Event, HistoricalAcknowledgement, HistoricalEvent,
    IssueVersion, Mapping, Observation, Product, PublicationScope, Receipt, ReceiptBody, Refusal,
    Release, RepairRequest, RepairSource, Sampling, SignedEvent,
};
use serde::{Deserialize, Serialize};

fn any<T: Arbitrary>(rng: &mut Rng) -> T {
    T::arbitrary(rng)
}

fn counter(rng: &mut Rng) -> Counter {
    Counter(any(rng))
}

fn endpoint(rng: &mut Rng) -> Endpoint {
    Endpoint {
        product: *rng.pick(&[Product::Alt, Product::Sentori]),
        installation: any(rng),
        workspace: any(rng),
        scope: any(rng),
        generation: counter(rng),
    }
}

fn release(rng: &mut Rng) -> Release {
    Release {
        application: any(rng),
        release: any(rng),
        release_name: any(rng),
        environment: any(rng),
        artifact_sha256: any(rng),
    }
}

fn delivery(rng: &mut Rng) -> Delivery {
    Delivery {
        source: PublicationScope {
            installation: any(rng),
            repository: any(rng),
            epoch: counter(rng),
        },
        provenance_digest: any(rng),
        publication_digest: any(rng),
        receipt_digest: any(rng),
        task: any(rng),
        integration: any(rng),
        delivery: any(rng),
        native_commit: any(rng),
        git_commit: any(rng),
    }
}

fn body(rng: &mut Rng) -> Body {
    match rng.below(5) {
        0 => Body::Mapping(Mapping {
            delivery: delivery(rng),
            release: release(rng),
            declared_by: any(rng),
        }),
        1 => Body::Withdraw {
            previous_mapping_digest: any(rng),
            reason: any(rng),
            declared_by: any(rng),
        },
        2 => Body::RepairRequest(RepairRequest {
            issue: any(rng),
            selected_samples: any(rng),
            observed_version: IssueVersion {
                application: any(rng),
                release: any(rng),
                release_name: any(rng),
                environment: any(rng),
            },
            mapping_event_digest: any(rng),
            target_ref: any(rng),
            requested_by: any(rng),
        }),
        3 => Body::TaskBound {
            repair_request_digest: any(rng),
            task: any(rng),
            publication_digest: any(rng),
        },
        _ => Body::Observation(Observation {
            mapping_event_digest: any(rng),
            release: release(rng),
            start_unix_ms: counter(rng),
            end_unix_ms: counter(rng),
            collected_unix_ms: counter(rng),
            persisted_events: counter(rng),
            persisted_error_events: counter(rng),
            persisted_sessions: counter(rng),
            sampling: Sampling::PersistedOnlyUnknownCoverage,
        }),
    }
}

fn signed_event(rng: &mut Rng) -> SignedEvent {
    SignedEvent {
        event: Event {
            version: any(rng),
            channel: any(rng),
            issuer: endpoint(rng),
            receiver: endpoint(rng),
            entity: any(rng),
            revision: counter(rng),
            previous_digest: any(rng),
            issued_unix_ms: counter(rng),
            body: body(rng),
        },
        key_id: any(rng),
        signature: any(rng),
    }
}

fn decision(rng: &mut Rng) -> Decision {
    let refusals = [
        Refusal::TargetUnavailable,
        Refusal::ArtifactUnavailable,
        Refusal::StateConflict,
        Refusal::RequestUnavailable,
    ];
    match rng.below(3) {
        0 => Decision::Applied,
        _ => Decision::Refused(*rng.pick(&refusals)),
    }
}

fn receipt(rng: &mut Rng) -> Receipt {
    Receipt {
        body: ReceiptBody {
            version: any(rng),
            channel: any(rng),
            issuer: endpoint(rng),
            receiver: endpoint(rng),
            entity: any(rng),
            revision: counter(rng),
            event_digest: any(rng),
            accepted_unix_ms: counter(rng),
            decision: decision(rng),
        },
        key_id: any(rng),
        signature: any(rng),
    }
}

fn repair_source(rng: &mut Rng) -> RepairSource {
    RepairSource {
        request: signed_event(rng),
        source_public_key: any(rng),
        acceptance: receipt(rng),
        receiver_public_key: any(rng),
    }
}

fn historical_event(rng: &mut Rng) -> HistoricalEvent {
    HistoricalEvent {
        statement: signed_event(rng),
        source_public_key: any(rng),
        acknowledgement: (!rng.chance(3)).then(|| HistoricalAcknowledgement {
            receipt: receipt(rng),
            receiver_public_key: any(rng),
        }),
    }
}

#[test]
fn embedded_types() {
    let mut total = Tally::default();
    let mut add = |name: &str, tally: Tally| {
        tally.report(name);
        total.add(&tally);
    };
    add(
        "bridge, change-link Counter",
        exercise_bridged(0xb1, 3000, counter),
    );
    add(
        "bridge, change-link Endpoint",
        exercise_bridged(0xb2, 3000, endpoint),
    );
    add(
        "bridge, change-link Release",
        exercise_bridged(0xb3, 3000, release),
    );
    add(
        "bridge, change-link Decision",
        exercise_bridged(0xb4, 3000, decision),
    );
    add(
        "bridge, change-link Body",
        exercise_bridged(0xb5, 3000, body),
    );
    add(
        "bridge, change-link SignedEvent",
        exercise_bridged(0xb6, 1500, signed_event),
    );
    add(
        "bridge, change-link Receipt",
        exercise_bridged(0xb7, 1500, receipt),
    );
    add(
        "bridge, change-link RepairSource",
        exercise_bridged(0xb8, 1000, repair_source),
    );
    add(
        "bridge, change-link HistoricalEvent",
        exercise_bridged(0xb9, 1000, historical_event),
    );
    total.report("bridge, change-link types in total");
}

/// A record of ours with vendored fields, as alt-task and the remote
/// protocol have them.
#[derive(Debug, Serialize, Deserialize, ToJson, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
struct Holder {
    purpose: String,
    #[json(with = "alt_json_serde::bridge")]
    release: Release,
    #[json(with = "alt_json_serde::bridge")]
    peer: Endpoint,
    #[json(with = "alt_json_serde::bridge")]
    revision: Counter,
    #[json(default, with = "alt_json_serde::bridge")]
    decision: Option<Decision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[json(
        default,
        skip_serializing_if = "Option::is_none",
        with = "alt_json_serde::bridge"
    )]
    repair_source: Option<Box<RepairSource>>,
    last: u8,
}

#[derive(Debug, Serialize, Deserialize, ToJson, FromJson)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[json(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Tagged {
    Mapping {
        delivery_id: String,
        #[json(with = "alt_json_serde::bridge")]
        release: Release,
    },
    Receipt(#[json(with = "alt_json_serde::bridge")] Receipt),
    Nothing,
}

#[test]
fn as_fields_of_derived_types() {
    let holder = |rng: &mut Rng| Holder {
        purpose: any(rng),
        release: release(rng),
        peer: endpoint(rng),
        revision: counter(rng),
        decision: (!rng.chance(3)).then(|| decision(rng)),
        repair_source: rng.chance(2).then(|| Box::new(repair_source(rng))),
        last: any(rng),
    };
    let tagged = |rng: &mut Rng| match rng.below(3) {
        0 => Tagged::Mapping {
            delivery_id: any(rng),
            release: release(rng),
        },
        1 => Tagged::Receipt(receipt(rng)),
        _ => Tagged::Nothing,
    };
    exercise_with(0xc1, 2000, holder).report("bridge, vendored fields in a derived struct");
    exercise_with(0xc2, 3000, tagged).report("bridge, vendored fields in a tagged enum");
}

/// A type with serde implementations only, in each form an enum variant
/// can take.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SerdeOnly {
    Unit,
    One(u8),
    Pair(u8, String),
    Fields { left: i8, right: Option<String> },
}

/// One of our enums with a tag, whose variants hold serde types directly.
#[derive(Debug, Serialize, Deserialize, ToJson, FromJson)]
#[serde(tag = "t", rename_all = "snake_case")]
#[json(tag = "t", rename_all = "snake_case")]
enum Nested {
    Inner(#[json(with = "alt_json_serde::bridge")] SerdeOnly),
    Decision(#[json(with = "alt_json_serde::bridge")] Decision),
    Release(#[json(with = "alt_json_serde::bridge")] Release),
    Plain { x: u8 },
}

#[test]
fn serde_enums_inside_a_tagged_variant() {
    let nested = |rng: &mut Rng| match rng.below(7) {
        0 => Nested::Inner(SerdeOnly::Unit),
        1 => Nested::Inner(SerdeOnly::One(any(rng))),
        2 => Nested::Inner(SerdeOnly::Pair(any(rng), any(rng))),
        3 => Nested::Inner(SerdeOnly::Fields {
            left: any(rng),
            right: any(rng),
        }),
        4 => Nested::Decision(decision(rng)),
        5 => Nested::Release(release(rng)),
        _ => Nested::Plain { x: any(rng) },
    };
    exercise_with(0xc4, 6000, nested).report("bridge, serde enums inside a tagged variant");
}

#[test]
fn trees_of_either_library() {
    let mut rng = Rng::new(0xc3);
    for _ in 0..20_000 {
        let theirs: serde_json::Value = any(&mut rng);
        let ours = alt_json_serde::to_value(&theirs).unwrap();
        assert_eq!(
            alt_json::to_vec(&ours).unwrap(),
            serde_json::to_vec(&theirs).unwrap()
        );
        let back: serde_json::Value = alt_json_serde::from_value(ours).unwrap();
        assert_eq!(back, theirs);
    }
    println!("bridge, tree conversion: 20000 cases");
}
