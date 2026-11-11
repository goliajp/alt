//! The configuration and backup types held to recorded bytes and verdicts.

use alt_json_testkit::fixtures::{assert_stored, verify_file};

use crate::change_links::ChannelConfig;
use crate::operator::Config;
use crate::operator::backup::{Manifest, SignedManifest};

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");

#[test]
fn recorded_bytes_and_verdicts() {
    verify_file::<Config>(DIR, "operator-Config");
    verify_file::<ChannelConfig>(DIR, "change_links-ChannelConfig");
    verify_file::<Manifest>(DIR, "backup-Manifest");
    verify_file::<SignedManifest>(DIR, "backup-SignedManifest");
}

/// The shipped configuration, in the compact and the indented form that
/// provisioning compares with the file it wrote earlier.
#[test]
fn shipped_configuration_is_written_as_recorded() {
    let config: Config = alt_json::from_str(include_str!("../example-config.json")).unwrap();
    let written = format!(
        "{}\n{}\n",
        alt_json::to_string(&config).unwrap(),
        alt_json::to_string_pretty(&config).unwrap()
    );
    assert_stored(&format!("{DIR}/example-config.txt"), &written);
}
