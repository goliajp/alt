//! The delivery records read and written through alt-json, compared with
//! serde on random values and held to recorded bytes, verdicts and
//! identities.

use alt_delivery::{ExportReport, FileEntry, GitCheck, IntakeReport, ObjectIdentity, Snapshot};
use alt_json_testkit::drive::exercise_with;
use alt_json_testkit::fixtures::{check_recorded, record, verify};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::support::Arbitrary;

fn any<T: Arbitrary>(rng: &mut Rng) -> T {
    T::arbitrary(rng)
}

fn object_identity(rng: &mut Rng) -> ObjectIdentity {
    ObjectIdentity {
        kind: any(rng),
        bytes: any(rng),
        blake3: any(rng),
    }
}

fn file_entry(rng: &mut Rng) -> FileEntry {
    FileEntry {
        mode: any(rng),
        oid: any(rng),
    }
}

fn snapshot(rng: &mut Rng) -> Snapshot {
    Snapshot {
        schema_version: any(rng),
        object_format: any(rng),
        objects: (0..rng.below(3))
            .map(|_| (any(rng), object_identity(rng)))
            .collect(),
        refs: any(rng),
        config_hex: any(rng),
    }
}

fn git_check(rng: &mut Rng) -> GitCheck {
    GitCheck {
        program: any(rng),
        binary_blake3: any(rng),
        version_stdout_hex: any(rng),
        version_stderr_hex: any(rng),
        fsck_stdout_hex: any(rng),
        fsck_stderr_hex: any(rng),
        fsck_exit: any(rng),
    }
}

fn intake_report(rng: &mut Rng) -> IntakeReport {
    IntakeReport {
        baseline: snapshot(rng),
        source_metadata_blake3: any(rng),
        git: git_check(rng),
    }
}

fn export_report(rng: &mut Rng) -> ExportReport {
    ExportReport {
        baseline_id: any(rng),
        exported: snapshot(rng),
        files: (0..rng.below(3))
            .map(|_| (any(rng), file_entry(rng)))
            .collect(),
        git: git_check(rng),
        config_transform: any(rng),
    }
}

#[test]
fn same_as_serde() {
    exercise_with(0xd0, 1500, object_identity).report("alt-delivery, ObjectIdentity");
    exercise_with(0xd1, 1500, file_entry).report("alt-delivery, FileEntry");
    exercise_with(0xd2, 1500, snapshot).report("alt-delivery, Snapshot");
    exercise_with(0xd3, 1500, git_check).report("alt-delivery, GitCheck");
    exercise_with(0xd4, 1000, intake_report).report("alt-delivery, IntakeReport");
    exercise_with(0xd5, 1000, export_report).report("alt-delivery, ExportReport");
}

macro_rules! fixture {
    ($file:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures/", $file)
    };
}

/// the baseline identity is the one the serde encoding gave
#[test]
fn identities_agree() {
    let mut rng = Rng::new(0xdd);
    let mut lines = Vec::new();
    for round in 0..20_000 {
        let value = snapshot(&mut rng);
        let id = alt_input::identity("delivery-baseline", &value).unwrap();
        assert_eq!(id, value.id().unwrap());
        let metadata: std::collections::BTreeMap<String, String> = any(&mut rng);
        assert_eq!(
            alt_input::identity("source-git-metadata", &metadata).unwrap(),
            alt_input::identity_json("source-git-metadata", &metadata).unwrap()
        );
        if round < 50 {
            lines.push(format!("{id} {}", serde_json::to_string(&value).unwrap()));
        }
    }
    check_recorded(fixture!("identities.txt"), &(lines.join("\n") + "\n"));
    println!(
        "alt-delivery identities: 40000 compared, {} recorded",
        lines.len()
    );
}

#[test]
fn recorded_bytes_and_verdicts() {
    check_recorded(fixture!("snapshot.txt"), &record(0xb0, 30, snapshot));
    check_recorded(
        fixture!("intake_report.txt"),
        &record(0xb1, 30, intake_report),
    );
    check_recorded(
        fixture!("export_report.txt"),
        &record(0xb2, 30, export_report),
    );
    let snapshots = verify::<Snapshot>(include_str!("fixtures/snapshot.txt"));
    let intakes = verify::<IntakeReport>(include_str!("fixtures/intake_report.txt"));
    let exports = verify::<ExportReport>(include_str!("fixtures/export_report.txt"));
    println!("alt-delivery fixtures: {snapshots:?} {intakes:?} {exports:?}");
    assert_eq!(
        (snapshots.written, intakes.written, exports.written),
        (30, 30, 30)
    );
}

/// the recorded identity of each recorded baseline, without serde
#[test]
fn recorded_identities() {
    let mut checked = 0;
    for line in include_str!("fixtures/identities.txt").lines() {
        let (id, text) = line.split_once(' ').unwrap();
        let value: Snapshot = alt_json::from_str(text).unwrap();
        assert_eq!(value.id().unwrap(), id);
        checked += 1;
    }
    assert_eq!(checked, 50);
}
