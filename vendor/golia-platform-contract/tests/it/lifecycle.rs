use super::{audience, id};
use golia_platform_contract::{Product, Secret, VERSION, lifecycle::*};
fn spec() -> ProvisionSpec {
    ProvisionSpec {
        version: VERSION,
        audience: audience(),
        region: id("region"),
        images: Images {
            application: format!("alt@sha256:{}", "a".repeat(64)),
            database: format!("postgres@sha256:{}", "b".repeat(64)),
            edge: Some(format!("edge@sha256:{}", "c".repeat(64))),
        },
        runtime_key: Secret::try_from("1".repeat(64)).unwrap(),
        data_key: Secret::try_from("2".repeat(64)).unwrap(),
        database_password: Secret::try_from("3".repeat(64)).unwrap(),
    }
}
#[test]
fn deployment_identity_images_and_secrets_are_frozen() {
    let s = spec();
    s.validate().unwrap();
    let digest = s.digest().unwrap();
    let value = serde_json::to_value(&s).unwrap();
    let decoded: ProvisionSpec = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded.digest().unwrap(), digest);
    let mut variants = vec![];
    let mut next = s.clone();
    next.audience.installation_id = id("elsewhere");
    variants.push(next);
    let mut next = s.clone();
    next.audience.generation.0 += 1;
    variants.push(next);
    let mut next = s.clone();
    next.region = id("other-region");
    variants.push(next);
    let mut next = s.clone();
    next.images.application = format!("alt@sha256:{}", "d".repeat(64));
    variants.push(next);
    let mut next = s.clone();
    next.runtime_key = Secret::try_from("4".repeat(64)).unwrap();
    variants.push(next);
    for next in variants {
        assert_ne!(next.digest().unwrap(), digest);
    }
    for image in [
        "alt:latest".to_owned(),
        "sha256:short".to_owned(),
        format!("alt@$sha256:{}", "a".repeat(64)),
    ] {
        let mut next = s.clone();
        next.images.application = image;
        assert!(next.validate().is_err());
    }
    let mut next = s.clone();
    next.data_key = next.runtime_key.clone();
    assert!(next.validate().is_err());
    let mut next = s.clone();
    next.images.edge = None;
    assert!(next.validate().is_err());
    next.audience.product = Product::Sentori;
    assert!(next.validate().is_ok());
    let mut value = value;
    value["command"] = serde_json::json!("unexpected");
    assert!(serde_json::from_value::<ProvisionSpec>(value).is_err());
}
#[test]
fn ready_requires_exact_fresh_probe_and_both_stores() {
    let s = spec();
    let probe = Probe {
        version: VERSION,
        audience: s.audience.clone(),
        spec_digest: s.digest().unwrap(),
        challenge: "f".repeat(64),
        recovery_manifest_digest: None,
    };
    let ready = Readiness {
        probe: probe.clone(),
        database: true,
        blob_storage: true,
        durable_storage: true,
    };
    ready.check(&probe).unwrap();
    let mut old = probe.clone();
    old.challenge = "e".repeat(64);
    assert!(ready.check(&old).is_err());
    let mut old = probe.clone();
    old.audience.generation.0 += 1;
    assert!(ready.check(&old).is_err());
    let mut old = probe.clone();
    old.spec_digest = "e".repeat(64);
    assert!(ready.check(&old).is_err());
    for (database, blob_storage, durable_storage) in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
    ] {
        assert!(
            Readiness {
                database,
                blob_storage,
                durable_storage,
                ..ready.clone()
            }
            .check(&probe)
            .is_err()
        );
    }
}

#[test]
fn recovery_is_same_installation_with_fresh_authority_and_exact_backup() {
    let source = spec();
    let mut target = source.clone();
    target.audience.generation.0 += 2;
    target.runtime_key = Secret::try_from("4".repeat(64)).unwrap();
    target.data_key = Secret::try_from("5".repeat(64)).unwrap();
    target.database_password = Secret::try_from("6".repeat(64)).unwrap();
    let recovery = RecoverySpec {
        version: VERSION,
        operation_id: id("operation"),
        backup: RecoveryBackup {
            manifest_digest: "a".repeat(64),
            receipt_public_key: "b".repeat(64),
            generation: source.audience.generation,
            maximum_bytes: crate::Counter(100_000),
        },
        source,
        target,
    };
    let digest = recovery.digest().unwrap();
    for change in [
        "org",
        "space",
        "instance",
        "origin",
        "control",
        "generation",
        "key",
        "data",
        "database",
        "region",
        "image",
        "backup",
    ] {
        let mut r = recovery.clone();
        match change {
            "org" => r.target.audience.organization_id = id("other"),
            "space" => r.target.audience.workspace_id = id("other"),
            "instance" => r.target.audience.installation_id = id("other"),
            "origin" => {
                r.target.audience.origin =
                    "https://other.example.test".to_string().try_into().unwrap()
            }
            "control" => {
                r.target.audience.control_origin =
                    "https://other.example.test".to_string().try_into().unwrap()
            }
            "generation" => r.target.audience.generation.0 -= 1,
            "key" => r.target.runtime_key = r.source.runtime_key.clone(),
            "data" => r.target.data_key = r.source.data_key.clone(),
            "database" => r.target.database_password = r.source.database_password.clone(),
            "region" => r.target.region = id("elsewhere"),
            "image" => r.target.images.application = format!("sha256:{}", "e".repeat(64)),
            "backup" => r.backup.generation.0 = r.target.audience.generation.0,
            _ => unreachable!(),
        }
        assert!(r.validate().is_err(), "{change}");
    }
    let mut other_backup = recovery.clone();
    other_backup.backup.manifest_digest = "c".repeat(64);
    assert_ne!(other_backup.digest().unwrap(), digest);
    let encoded = serde_json::to_vec(&recovery).unwrap();
    assert_eq!(
        serde_json::from_slice::<RecoverySpec>(&encoded)
            .unwrap()
            .digest()
            .unwrap(),
        digest
    );
}
