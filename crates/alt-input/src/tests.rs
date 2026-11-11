use super::*;
use std::collections::BTreeMap;

fn context() -> Context {
    Context {
        commands: vec![vec!["gate".into()]],
        cwd: ".".into(),
        environment: Environment {
            values: BTreeMap::new(),
            complete: true,
        },
        tools: vec![],
        external: vec![],
        validation: alt_json::json!({"required":"gate"}),
    }
}
fn plan() -> InputPlan {
    InputPlan {
        source: vec!["src.rs".into()],
        auxiliary: vec![InputRoot {
            path: ".ignored".into(),
            purpose: "probe and fixture".into(),
        }],
        exclusions: vec![".ignored/secret".into()],
    }
}
#[test]
fn all_selected_inputs_and_directory_members_change_identity() {
    let dir = alt_os::temp::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".ignored")).unwrap();
    for path in [
        "src.rs",
        ".ignored/probe",
        ".ignored/generator",
        ".ignored/fixture",
        ".ignored/secret",
    ] {
        std::fs::write(dir.path().join(path), b"old").unwrap();
    }
    let root = Root::open(dir.path()).unwrap();
    let before = scan(&root, &root, plan(), context()).unwrap();
    for path in [
        "src.rs",
        ".ignored/probe",
        ".ignored/generator",
        ".ignored/fixture",
    ] {
        std::fs::write(dir.path().join(path), b"changed").unwrap();
        assert_ne!(
            scan(&root, &root, plan(), context()).unwrap().id().unwrap(),
            before.id().unwrap(),
            "{path}"
        );
        std::fs::write(dir.path().join(path), b"old").unwrap();
    }
    std::fs::write(dir.path().join(".ignored/secret"), b"different secret").unwrap();
    assert_eq!(
        scan(&root, &root, plan(), context()).unwrap().id().unwrap(),
        before.id().unwrap()
    );
    std::fs::write(dir.path().join(".ignored/new-member"), b"new").unwrap();
    assert_ne!(
        scan(&root, &root, plan(), context()).unwrap().id().unwrap(),
        before.id().unwrap()
    );
    let mut changed = before.clone();
    changed.context.validation = alt_json::json!({"required":"another gate"});
    assert_ne!(changed.id().unwrap(), before.id().unwrap());
    changed = before.clone();
    changed
        .context
        .environment
        .values
        .insert("MODE".into(), "other".into());
    assert_ne!(changed.id().unwrap(), before.id().unwrap());
    changed
        .context
        .environment
        .values
        .insert("API_TOKEN".into(), "not-a-real-token".into());
    assert!(
        changed.id().is_err(),
        "credentials require provider references"
    );
}
#[test]
fn symlink_parents_and_escape_are_refused_without_reading_outside() {
    use std::os::unix::fs::symlink;
    let dir = alt_os::temp::tempdir().unwrap();
    let outside = alt_os::temp::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"never capture").unwrap();
    symlink(outside.path(), dir.path().join("linked")).unwrap();
    let root = Root::open(dir.path()).unwrap();
    assert!(root.inspect("linked/secret").is_err());
    assert!(root.inspect("linked").is_err());
    symlink("../../secret", dir.path().join("escape")).unwrap();
    assert!(root.inspect("escape").is_err());
}
#[test]
fn bytes_modes_and_path_validation_are_lossless() {
    use std::os::unix::fs::PermissionsExt;
    let dir = alt_os::temp::tempdir().unwrap();
    std::fs::write(dir.path().join("日本語"), [0, 255, 10]).unwrap();
    let root = Root::open(dir.path()).unwrap();
    let before = root.inspect("日本語").unwrap();
    std::fs::set_permissions(
        dir.path().join("日本語"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let after = root.inspect("日本語").unwrap();
    assert_eq!(before.content, after.content);
    assert!(!before.executable && after.executable);
    match checked_path(&[255]).unwrap_err() {
        Error::Path { bytes_hex, .. } => assert_eq!(bytes_hex, "ff"),
        other => panic!("{other}"),
    }
    for path in ["../escape", "a/../b", " space", "a\nb", "/absolute"] {
        assert!(checked_path(path.as_bytes()).is_err());
    }
}
#[test]
fn canonical_identity_does_not_depend_on_json_map_order() {
    let left: alt_json::Value = alt_json::from_str(r#"{"z":{"b":2,"a":1},"a":0}"#).unwrap();
    let right: alt_json::Value = alt_json::from_str(r#"{"a":0,"z":{"a":1,"b":2}}"#).unwrap();
    assert_eq!(
        identity_json("example", &left).unwrap(),
        identity_json("example", &right).unwrap()
    );
    assert_ne!(
        identity_json("example", &left).unwrap(),
        identity_json("different domain", &left).unwrap()
    );
}
#[test]
fn link_chain_cannot_hide_a_non_directory_behind_parent_components() {
    use std::os::unix::fs::symlink;
    let dir = alt_os::temp::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    symlink("sub", dir.path().join("link")).unwrap();
    symlink("link/../other", dir.path().join("indirect")).unwrap();
    let root = Root::open(dir.path()).unwrap();
    let plan = InputPlan {
        source: vec!["link".into(), "indirect".into()],
        auxiliary: vec![],
        exclusions: vec![],
    };
    assert!(scan(&root, &root, plan, context()).is_err());
}

#[test]
fn explicit_corpus_may_contain_git_metadata_as_fixture_data() {
    let dir = alt_os::temp::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("corpus/repo/.git")).unwrap();
    std::fs::write(dir.path().join("corpus/repo/.git/HEAD"), b"fixture bytes").unwrap();
    let root = Root::open(dir.path()).unwrap();
    let plan = InputPlan {
        source: vec![],
        auxiliary: vec![InputRoot {
            path: "corpus".into(),
            purpose: "explicit Git fixture".into(),
        }],
        exclusions: vec![],
    };
    let manifest = scan(&root, &root, plan, context()).unwrap();
    assert!(
        manifest
            .entries
            .iter()
            .any(|entry| entry.path == "corpus/repo/.git/HEAD")
    );
    let mut bad = manifest.plan;
    bad.auxiliary[0].path = ".git".into();
    assert!(scan(&root, &root, bad, context()).is_err());
}
