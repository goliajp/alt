//! The manifest types read and written through alt-json, compared with
//! serde on random values and held to recorded bytes, verdicts and
//! identities.

use alt_input::{
    Context, Entry, EntryKind, Environment, ExternalDependency, InputPlan, InputRole, InputRoot,
    Manifest, Node, ToolIdentity, identity, identity_json,
};
use alt_json_testkit::drive::exercise_with;
use alt_json_testkit::fixtures::{check_recorded, record, verify};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::support::Arbitrary;

fn any<T: Arbitrary>(rng: &mut Rng) -> T {
    T::arbitrary(rng)
}

fn several<T>(rng: &mut Rng, make: fn(&mut Rng) -> T) -> Vec<T> {
    (0..rng.below(3)).map(|_| make(rng)).collect()
}

fn entry_kind(rng: &mut Rng) -> EntryKind {
    rng.pick(&[EntryKind::File, EntryKind::Symlink, EntryKind::Directory])
        .clone()
}

fn input_role(rng: &mut Rng) -> InputRole {
    rng.pick(&[InputRole::Source, InputRole::Auxiliary]).clone()
}

fn node(rng: &mut Rng) -> Node {
    Node {
        kind: entry_kind(rng),
        executable: any(rng),
        len: any(rng),
        content: any(rng),
        link_target: any(rng),
    }
}

fn entry(rng: &mut Rng) -> Entry {
    Entry {
        path: any(rng),
        path_bytes_hex: any(rng),
        role: input_role(rng),
        node: node(rng),
    }
}

fn input_root(rng: &mut Rng) -> InputRoot {
    InputRoot {
        path: any(rng),
        purpose: any(rng),
    }
}

fn input_plan(rng: &mut Rng) -> InputPlan {
    InputPlan {
        source: any(rng),
        auxiliary: several(rng, input_root),
        exclusions: any(rng),
    }
}

fn environment(rng: &mut Rng) -> Environment {
    Environment {
        values: any(rng),
        complete: any(rng),
    }
}

fn tool(rng: &mut Rng) -> ToolIdentity {
    ToolIdentity {
        name: any(rng),
        version: any(rng),
        binary_blake3: any(rng),
    }
}

fn external(rng: &mut Rng) -> ExternalDependency {
    ExternalDependency {
        provider: any(rng),
        version: any(rng),
        purpose: any(rng),
    }
}

fn context(rng: &mut Rng) -> Context {
    Context {
        commands: any(rng),
        cwd: any(rng),
        environment: environment(rng),
        tools: several(rng, tool),
        external: several(rng, external),
        validation: any(rng),
    }
}

fn manifest(rng: &mut Rng) -> Manifest {
    Manifest {
        schema_version: any(rng),
        hash_algorithm: any(rng),
        plan: input_plan(rng),
        context: context(rng),
        entries: several(rng, entry),
    }
}

#[test]
fn same_as_serde() {
    exercise_with(0x10, 1500, entry_kind).report("alt-input, EntryKind");
    exercise_with(0x11, 1500, input_role).report("alt-input, InputRole");
    exercise_with(0x12, 1500, node).report("alt-input, Node");
    exercise_with(0x13, 1500, entry).report("alt-input, Entry");
    exercise_with(0x14, 1500, input_root).report("alt-input, InputRoot");
    exercise_with(0x15, 1500, input_plan).report("alt-input, InputPlan");
    exercise_with(0x16, 1500, environment).report("alt-input, Environment");
    exercise_with(0x17, 1500, tool).report("alt-input, ToolIdentity");
    exercise_with(0x18, 1500, external).report("alt-input, ExternalDependency");
    exercise_with(0x19, 1500, context).report("alt-input, Context");
    exercise_with(0x1a, 800, manifest).report("alt-input, Manifest");
}

/// both identity functions hash the same bytes
#[test]
fn identities_agree() {
    let mut rng = Rng::new(0x1d);
    let mut lines = Vec::new();
    for round in 0..20_000 {
        let value = manifest(&mut rng);
        let id = identity("manifest", &value).unwrap();
        assert_eq!(id, identity_json("manifest", &value).unwrap());
        assert_eq!(
            identity("context", &value.context).unwrap(),
            identity_json("context", &value.context).unwrap()
        );
        if round < 50 {
            // the identity of the manifest as it reads back from its text
            let text = serde_json::to_string(&value).unwrap();
            let reread: Manifest = serde_json::from_str(&text).unwrap();
            lines.push(format!("{} {text}", identity("manifest", &reread).unwrap()));
        }
    }
    check_recorded(fixture!("identities.txt"), &(lines.join("\n") + "\n"));
    println!(
        "alt-input identities: 40000 compared, {} recorded",
        lines.len()
    );
}

macro_rules! fixture {
    ($file:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures/", $file)
    };
}
use fixture;

#[test]
fn recorded_bytes_and_verdicts() {
    check_recorded(fixture!("manifest.txt"), &record(0xa0, 30, manifest));
    check_recorded(fixture!("context.txt"), &record(0xa1, 30, context));
    check_recorded(fixture!("entry.txt"), &record(0xa2, 30, entry));
    let manifests = verify::<Manifest>(include_str!("fixtures/manifest.txt"));
    let contexts = verify::<Context>(include_str!("fixtures/context.txt"));
    let entries = verify::<Entry>(include_str!("fixtures/entry.txt"));
    println!("alt-input fixtures: {manifests:?} {contexts:?} {entries:?}");
    assert_eq!(
        (manifests.written, contexts.written, entries.written),
        (30, 29, 30)
    );
}

/// the recorded identity of each recorded manifest, without serde
#[test]
fn recorded_identities() {
    let mut checked = 0;
    for line in include_str!("fixtures/identities.txt").lines() {
        let (id, text) = line.split_once(' ').unwrap();
        let value: Manifest = alt_json::from_str(text).unwrap();
        assert_eq!(identity_json("manifest", &value).unwrap(), id);
        checked += 1;
    }
    assert_eq!(checked, 50);
}
