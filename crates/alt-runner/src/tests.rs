use super::*;
use alt_input::{Environment, InputPlan, InputRoot, Root};
use std::path::PathBuf;

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    spec: Spec,
}
fn fixture(script: &str) -> Fixture {
    // /tmp is often tmpfs on Linux; the product support contract is local disk.
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let source = directory.path().join("source");
    let root = directory.path().join("run");
    fs::create_dir_all(source.join(".ignored")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(source.join("gate.sh"), script).unwrap();
    fs::write(source.join("source.txt"), "input\n").unwrap();
    fs::write(source.join(".ignored/probe"), "probe\n").unwrap();
    let environment = Environment {
        values: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        complete: true,
    };
    let binding = ToolBinding {
        name: "bash".into(),
        path: "/bin/bash".into(),
        version_args: vec!["--version".into()],
    };
    let identity = probe(&source, &binding, &environment.values).unwrap();
    let context = Context {
        commands: vec![vec!["bash".into(), "gate.sh".into()]],
        cwd: ".".into(),
        environment,
        tools: vec![identity],
        external: Vec::new(),
        validation: serde_json::json!({"outputs":["target"]}),
    };
    let tree = Root::open(&source).unwrap();
    let manifest = alt_input::scan(
        &tree,
        &tree,
        InputPlan {
            source: vec!["gate.sh".into(), "source.txt".into()],
            auxiliary: vec![InputRoot {
                path: ".ignored".into(),
                purpose: "probe".into(),
            }],
            exclusions: Vec::new(),
        },
        context,
    )
    .unwrap();
    let blobs: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .filter(|entry| entry.node.kind == alt_input::EntryKind::File)
        .map(|entry| {
            (
                entry.node.content.clone(),
                fs::read(source.join(&entry.path)).unwrap(),
            )
        })
        .collect();
    materialize(&root, &manifest, |id, _| Ok(blobs[id].clone())).unwrap();
    fs::create_dir(root.join("target")).unwrap();
    let spec = Spec {
        schema_version: 1,
        candidate: "0".repeat(64),
        nonce: "1".repeat(32),
        manifest,
        tools: vec![binding],
        timeout_ms: 5000,
        runner: "unit-test".into(),
    };
    Fixture {
        _directory: directory,
        root,
        spec,
    }
}

#[test]
fn complete_run_preserves_binary_logs_and_rejects_same_nonce_reexecution() {
    let f = fixture(
        "printf 'before\\377\\000after\\n'; printf 'diagnostic\\n' >&2; printf output > target/result\n",
    );
    let report = execute(&f.root, &f.spec, &[7; 32]).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.evidence.termination, Termination::Exit { code: 0 });
    assert_eq!(report.evidence.before, report.evidence.after);
    let stdout = report.evidence.stdout.as_ref().unwrap();
    assert_eq!(
        fs::read(f.root.join(CONTROL).join(&report.captures[&stdout.blake3])).unwrap(),
        b"before\xff\0after\n"
    );
    for artifact in report
        .evidence
        .binaries
        .values()
        .chain(report.evidence.artifacts.values())
    {
        let bytes = fs::read(
            f.root
                .join(CONTROL)
                .join(&report.captures[&artifact.blake3]),
        )
        .unwrap();
        assert_eq!(
            alt_hash::blake3::hash(&bytes).to_hex().as_str(),
            artifact.blake3
        );
        assert_eq!(bytes.len() as u64, artifact.bytes);
    }
    assert!(execute(&f.root, &f.spec, &[7; 32]).is_err());
}

#[test]
fn actual_disk_rewrites_and_new_files_cannot_reuse_the_old_input_identity() {
    for script in [
        "printf changed > source.txt\n",
        "printf changed > .ignored/probe\n",
        "printf new > .ignored/new\n",
        "printf new > surprise.rs\n",
        "mkdir .alt; printf new > .alt/unexpected\n",
    ] {
        let f = fixture(script);
        let report = execute(&f.root, &f.spec, &[7; 32]).unwrap();
        assert_eq!(report.evidence.termination, Termination::Exit { code: 0 });
        assert_eq!(
            report.evidence.before.as_deref(),
            Some(f.spec.manifest.id().unwrap().as_str())
        );
        assert!(report.evidence.after.is_none(), "{script}");
        assert!(report.problems.contains(&"inputs_after_mismatch".into()));
    }
}

#[test]
fn nonzero_signal_and_timeout_keep_their_distinct_termination() {
    for (script, expected) in [
        ("printf failed; exit 9\n", Termination::Exit { code: 9 }),
        (
            "printf signaled; kill -TERM $$\n",
            Termination::Signal {
                number: libc::SIGTERM,
            },
        ),
        (
            "printf waiting; while :; do :; done\n",
            Termination::Timeout,
        ),
    ] {
        let mut f = fixture(script);
        f.spec.timeout_ms = 100;
        let report = execute(&f.root, &f.spec, &[7; 32]).unwrap();
        assert_eq!(report.evidence.termination, expected);
        assert!(report.evidence.stdout.as_ref().unwrap().bytes > 0);
        assert!(report.evidence.stderr.is_some());
    }
}

#[test]
fn stale_tool_and_unknown_environment_prevent_execution() {
    for change_tool in [true, false] {
        let mut f = fixture("printf must-not-run > target/ran\n");
        if change_tool {
            f.spec.manifest.context.tools[0].binary_blake3 = Some("0".repeat(64));
        } else {
            f.spec.manifest.context.environment.complete = false;
        }
        let report = execute(&f.root, &f.spec, &[7; 32]).unwrap();
        assert!(!report.problems.is_empty());
        assert_eq!(report.evidence.termination, Termination::Unknown);
        assert!(!f.root.join("target/ran").exists());
    }
}

#[test]
fn declared_outputs_cannot_hide_input_changes() {
    let mut f = fixture("printf ok\n");
    for outputs in [
        serde_json::json!([".ignored"]),
        serde_json::json!(["source.txt"]),
        serde_json::json!(["."]),
        serde_json::json!([CONTROL]),
        serde_json::json!(["target", "target/nested"]),
    ] {
        f.spec.manifest.context.validation["outputs"] = outputs;
        assert!(output_roots(&f.spec.manifest).is_err());
    }
}

#[test]
fn missing_logs_do_not_yield_a_complete_report() {
    let f = fixture("/bin/rm .alt-runner/stdout.bin\n");
    assert!(execute(&f.root, &f.spec, &[7; 32]).is_err());
    assert!(f.root.join(CONTROL).join("started.json").exists());
    assert!(!f.root.join(CONTROL).join("report.json").exists());
}

#[test]
fn external_versions_require_an_observation_in_the_acceptance_definition() {
    for probe_defined in [false, true] {
        let mut f = fixture("printf checked\n");
        f.spec
            .manifest
            .context
            .external
            .push(alt_input::ExternalDependency {
                provider: "registry-snapshot".into(),
                version: Some("v1".into()),
                purpose: "offline registry".into(),
            });
        if probe_defined {
            f.spec.manifest.context.validation["external_probes"] =
                serde_json::json!({"registry-snapshot":["bash","-c","printf v1"]});
        }
        let report = execute(&f.root, &f.spec, &[7; 32]).unwrap();
        if probe_defined {
            assert!(report.problems.is_empty(), "{:?}", report.problems);
            assert_eq!(
                report.evidence.actual_context.external[0]
                    .version
                    .as_deref(),
                Some("v1")
            );
            assert!(
                report
                    .evidence
                    .artifacts
                    .contains_key("external:before:registry-snapshot:stdout")
            );
            assert!(
                report
                    .evidence
                    .artifacts
                    .contains_key("external:after:registry-snapshot:stdout")
            );
        } else {
            assert!(report.evidence.actual_context.external[0].version.is_none());
            assert_eq!(report.evidence.termination, Termination::Unknown);
        }
    }
}

#[test]
fn root_auxiliary_bundle_restores_only_missing_verified_auxiliary_files() {
    for corrupt_transfer in [false, true] {
        let mut f = fixture("test -f root-probe && printf checked\n");
        let bytes = b"root auxiliary bytes\n";
        fs::write(f.root.join("root-probe"), bytes).unwrap();
        f.spec.manifest.plan.auxiliary.push(InputRoot {
            path: "root-probe".into(),
            purpose: "root file".into(),
        });
        let reader = Root::open(&f.root).unwrap();
        f.spec.manifest = alt_input::scan(
            &reader,
            &reader,
            f.spec.manifest.plan.clone(),
            f.spec.manifest.context.clone(),
        )
        .unwrap();
        fs::create_dir_all(f.root.join(CONTROL).join("auxiliary")).unwrap();
        fs::write(
            f.root
                .join(CONTROL)
                .join("auxiliary")
                .join(alt_hash::blake3::hash(bytes).to_hex().as_str()),
            bytes,
        )
        .unwrap();
        if corrupt_transfer {
            fs::write(f.root.join("root-probe"), "wrong received bytes").unwrap();
        } else {
            fs::remove_file(f.root.join("root-probe")).unwrap();
        }
        let observed = execute(&f.root, &f.spec, &[7; 32]);
        if corrupt_transfer {
            assert!(observed.is_err());
        } else {
            let report = observed.unwrap();
            assert!(report.problems.is_empty(), "{:?}", report.problems);
            assert_eq!(fs::read(f.root.join("root-probe")).unwrap(), bytes);
        }
    }
}

#[test]
fn failed_version_probe_preserves_original_streams_and_prevents_commands() {
    let mut f = fixture("printf should-not-run > target/ran\n");
    f.spec.tools[0].version_args = vec![
        "-c".into(),
        "printf 'version\\377\\000'; printf diagnostic >&2; exit 9".into(),
    ];
    let report = execute(&f.root, &f.spec, &[7; 32]).unwrap();
    assert_eq!(report.evidence.termination, Termination::Unknown);
    assert!(!f.root.join("target/ran").exists());
    let output = &report.evidence.artifacts["tool-probe:before:bash:stdout"];
    assert_eq!(
        fs::read(f.root.join(CONTROL).join(&report.captures[&output.blake3])).unwrap(),
        b"version\xff\0"
    );
    assert!(
        report
            .evidence
            .artifacts
            .contains_key("tool-probe:after:bash:termination")
    );
}
