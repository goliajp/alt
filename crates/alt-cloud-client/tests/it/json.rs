//! The platform contract types this client sends and receives, driven
//! through the serde bridge and compared with serde_json; and the request
//! bodies and sealing input, compared byte for byte with how they were
//! built before.

use alt_json_serde::Serde;
use alt_json_testkit::drive::exercise_bridged;
use alt_json_testkit::rng::Rng;
use alt_json_testkit::support::Tally;
use golia_platform_contract::navigation::{Entry, Navigation};
use golia_platform_contract::{
    AccountAuthorization, AltLimits, AltServiceAuthorization, Audience, Capability, Counter, Id,
    Lease, Product, Secret, TicketExchange, VERSION,
};

fn counter(rng: &mut Rng) -> Counter {
    Counter(match rng.below(4) {
        0 => u64::MAX,
        1 => 1 << 53,
        _ => rng.next_u64() >> rng.below(64),
    })
}

fn id(rng: &mut Rng) -> Id {
    let name = format!(
        "{}-{}",
        rng.pick(&["org", "space", "install", "account", "s"]),
        rng.below(100_000)
    );
    name.try_into().unwrap()
}

fn secret(rng: &mut Rng) -> Secret {
    crate::secret(&format!("{:016x}", rng.next_u64()).repeat(4))
}

fn audience(rng: &mut Rng) -> Audience {
    Audience {
        organization_id: id(rng),
        workspace_id: id(rng),
        installation_id: id(rng),
        product: *rng.pick(&[Product::Alt, Product::Sentori]),
        generation: counter(rng),
        origin: format!("https://alt-{}.example.test", rng.below(1000))
            .try_into()
            .unwrap(),
        control_origin: format!("https://control-{}.example.test", rng.below(1000))
            .try_into()
            .unwrap(),
    }
}

fn lease(rng: &mut Rng) -> Lease {
    Lease {
        version: VERSION,
        audience: audience(rng),
        sequence: counter(rng),
        revocation_generation: counter(rng),
        issued_at_unix_ms: counter(rng),
        expires_at_unix_ms: counter(rng),
    }
}

fn account(rng: &mut Rng) -> AccountAuthorization {
    AccountAuthorization {
        lease: lease(rng),
        account_id: id(rng),
        session_id: id(rng),
        membership_id: id(rng),
        display_name: format!("Name {}", rng.below(100)),
        capabilities: (0..rng.below(3))
            .map(|_| *rng.pick(&[Capability::UseProduct, Capability::ManageInstance]))
            .collect(),
    }
}

fn service(rng: &mut Rng) -> AltServiceAuthorization {
    AltServiceAuthorization {
        lease: lease(rng),
        writes_enabled: rng.chance(2),
        limits: AltLimits {
            repositories: counter(rng),
            stored_bytes: counter(rng),
            monthly_ingress_bytes: counter(rng),
        },
    }
}

fn navigation(rng: &mut Rng) -> Navigation {
    Navigation {
        account: account(rng),
        workspace_name: format!("Workspace {}", rng.below(100)),
        entries: (0..rng.below(3))
            .map(|_| Entry {
                audience: audience(rng),
                name: format!("Entry {}", rng.below(100)),
            })
            .collect(),
    }
}

fn ticket_exchange(rng: &mut Rng) -> TicketExchange {
    TicketExchange {
        version: VERSION,
        audience: audience(rng),
        ticket: secret(rng),
        verifier: secret(rng),
    }
}

#[test]
fn contract_types_through_the_bridge() {
    let mut total = Tally::default();
    let mut add = |name: &str, tally: Tally| {
        tally.report(name);
        total.add(&tally);
    };
    add(
        "bridge, platform Counter",
        exercise_bridged(0xa1, 2000, counter),
    );
    add("bridge, platform Id", exercise_bridged(0xa2, 2000, id));
    add(
        "bridge, platform Secret",
        exercise_bridged(0xa3, 2000, secret),
    );
    add(
        "bridge, platform Audience",
        exercise_bridged(0xa4, 2000, audience),
    );
    add(
        "bridge, platform Lease",
        exercise_bridged(0xa5, 1500, lease),
    );
    add(
        "bridge, platform AccountAuthorization",
        exercise_bridged(0xa6, 1500, account),
    );
    add(
        "bridge, platform AltServiceAuthorization",
        exercise_bridged(0xa7, 1500, service),
    );
    add(
        "bridge, platform Navigation",
        exercise_bridged(0xa8, 1000, navigation),
    );
    add(
        "bridge, platform TicketExchange",
        exercise_bridged(0xa9, 1500, ticket_exchange),
    );
    total.report("bridge, platform contract types in total");
}

/// What the client sends, and the associated data a sealed grant is bound
/// to, one line per round.
fn request_bodies(rounds: u64) -> String {
    let mut rng = Rng::new(0xab);
    let mut lines = String::new();
    for _ in 0..rounds {
        let audience = audience(&mut rng);
        let grant = secret(&mut rng);
        let digest = format!("{:064x}", rng.next_u64());
        let exchange = ticket_exchange(&mut rng);
        let parts = [
            alt_json::to_vec(&alt_json::json!({"version": VERSION, "audience": Serde(&audience), "grant": Serde(&grant)})).unwrap(),
            alt_json::to_vec(&alt_json::json!({"version": VERSION, "audience": Serde(&audience)})).unwrap(),
            alt_json::to_vec(&(VERSION, Serde(&audience), digest.as_str())).unwrap(),
            alt_json::to_vec(&Serde(&exchange)).unwrap(),
        ];
        for part in parts {
            lines += std::str::from_utf8(&part).unwrap();
            lines.push('\t');
        }
        lines.push('\n');
    }
    lines
}

#[test]
fn request_bodies_and_sealing_input_are_the_recorded_bytes() {
    alt_json_testkit::fixtures::assert_stored(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/it/fixtures/request-bodies.txt"
        ),
        &request_bodies(40),
    );
}
