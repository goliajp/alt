//! The configuration and backup types read and written through alt-json,
//! compared with serde on random values and held to recorded bytes and
//! verdicts.

use alt_json::json;
use alt_json_testkit::arbitrary::{Override, from_serde_with};
use alt_json_testkit::drive::exercise_with;
use alt_json_testkit::fixtures::{check_recorded, record, verify_file};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::support::{Arbitrary, Comparable, Tally};

use crate::change_links::ChannelConfig;
use crate::operator::backup::{FileDigest, Manifest, SignedManifest};
use crate::operator::{BrowserConfig, CloudConfig, Config, HttpBounds};

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");

fn name(rng: &mut Rng) -> &'static str {
    rng.pick(&["org", "workspace", "installation", "a-1", "b_2"])
}

// fields whose text the type validates while reading, and enums with a tag

fn listen(rng: &mut Rng) -> alt_json::Value {
    json!(std::net::SocketAddr::arbitrary(rng))
}

fn audience(rng: &mut Rng) -> alt_json::Value {
    json!({
        "organization_id": name(rng),
        "workspace_id": name(rng),
        "installation_id": name(rng),
        "product": "alt",
        "generation": (1 + rng.below(1 << 40)).to_string(),
        "origin": *rng.pick(&["https://alt.example.test", "https://a.example.test:8443"]),
        "control_origin": "https://cloud.example.test",
    })
}

fn target(rng: &mut Rng) -> alt_json::Value {
    let policy = match rng.below(2) {
        0 => json!({"kind": "public"}),
        _ => json!({
            "kind": "pinned",
            "addresses": (["127.0.0.1", "::1"][..1 + rng.below(2) as usize]),
            "allow_plaintext": rng.chance(2),
        }),
    };
    json!({"origin": String::arbitrary(rng), "policy": policy})
}

const OVERRIDES: &[Override] = &[
    ("listen", listen),
    ("audience", audience),
    ("target", target),
];

fn make<T: serde::de::DeserializeOwned>(rng: &mut Rng) -> T {
    from_serde_with(rng, OVERRIDES)
}

#[test]
fn same_as_serde() {
    let mut total = Tally::default();
    fn one<T: Comparable>(name: &str, seed: u64, total: &mut Tally) {
        let tally = exercise_with(seed, 300, make::<T>);
        tally.report(name);
        total.add(&tally);
    }
    one::<Config>("Config", 0xd100, &mut total);
    one::<CloudConfig>("CloudConfig", 0xd101, &mut total);
    one::<BrowserConfig>("BrowserConfig", 0xd102, &mut total);
    one::<HttpBounds>("HttpBounds", 0xd103, &mut total);
    one::<ChannelConfig>("ChannelConfig", 0xd104, &mut total);
    one::<FileDigest>("FileDigest", 0xd105, &mut total);
    one::<Manifest>("Manifest", 0xd106, &mut total);
    one::<SignedManifest>("SignedManifest", 0xd107, &mut total);
    total.report("alt-server configuration and backup, 8 types in total");
}

#[test]
fn recorded_bytes_and_verdicts() {
    fn one<T: Comparable>(name: &str, seed: u64) {
        check_recorded(&format!("{DIR}/{name}.txt"), &record(seed, 15, make::<T>));
        verify_file::<T>(DIR, name);
    }
    one::<Config>("operator-Config", 0xd110);
    one::<ChannelConfig>("change_links-ChannelConfig", 0xd111);
    one::<Manifest>("backup-Manifest", 0xd112);
    one::<SignedManifest>("backup-SignedManifest", 0xd113);
}

/// The shipped configuration, in the compact and the indented form that
/// provisioning compares with the file it wrote earlier.
#[test]
fn shipped_configuration_is_written_as_recorded() {
    let text = include_str!("../example-config.json");
    let config: Config = alt_json::from_str(text).unwrap();
    let theirs: Config = serde_json::from_str(text).unwrap();
    assert_eq!(
        alt_json::to_vec_pretty(&config).unwrap(),
        serde_json::to_vec_pretty(&theirs).unwrap()
    );
    let recorded = format!(
        "{}\n{}\n",
        serde_json::to_string(&theirs).unwrap(),
        serde_json::to_string_pretty(&theirs).unwrap()
    );
    check_recorded(&format!("{DIR}/example-config.txt"), &recorded);
    assert_eq!(
        format!(
            "{}\n{}\n",
            alt_json::to_string(&config).unwrap(),
            alt_json::to_string_pretty(&config).unwrap()
        ),
        recorded
    );
}
