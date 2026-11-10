//! Cross the persisted Cloud lease boundary during actual HTTP validation.
//! The protocol provider and the database controller stay outside the paused
//! server process. No product failpoints or locally extended leases are used.
use super::*;
use golia_platform_contract::{
    AccountAuthorization, AltLimits, AltServiceAuthorization, Audience, Capability, Counter, Lease,
    Product, Secret,
};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize},
    },
    time::Instant,
};
use tiny_http::{Header, Response, Server};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn secret(value: &str) -> Secret {
    value.to_owned().try_into().unwrap()
}
fn lease(audience: &Audience, sequence: u64) -> Lease {
    Lease {
        version: 1,
        audience: audience.clone(),
        sequence: Counter(sequence),
        revocation_generation: Counter(1),
        issued_at_unix_ms: Counter(now()),
        expires_at_unix_ms: Counter(now() + 60_000),
    }
}
fn account(audience: &Audience, sequence: u64) -> AccountAuthorization {
    AccountAuthorization {
        lease: lease(audience, sequence),
        account_id: "owner".to_string().try_into().unwrap(),
        session_id: "session".to_string().try_into().unwrap(),
        membership_id: "membership".to_string().try_into().unwrap(),
        display_name: "Owner".into(),
        capabilities: vec![Capability::UseProduct, Capability::ManageInstance],
    }
}
fn service(audience: &Audience, sequence: u64, writes: bool) -> AltServiceAuthorization {
    AltServiceAuthorization {
        lease: lease(audience, sequence),
        writes_enabled: writes,
        limits: AltLimits {
            repositories: Counter(4),
            stored_bytes: Counter(100_000_000),
            monthly_ingress_bytes: Counter(200_000_000),
        },
    }
}
struct Provider {
    audience: Audience,
    mode: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Provider {
    fn new(f: &Fixture) -> Self {
        let server = Server::http("127.0.0.1:0").unwrap();
        let audience = Audience {
            organization_id: "organization".to_string().try_into().unwrap(),
            workspace_id: f.spec.workspace_id.clone().try_into().unwrap(),
            installation_id: f.spec.installation_id.clone().try_into().unwrap(),
            product: Product::Alt,
            generation: Counter(1),
            origin: "https://alt.example.test".to_string().try_into().unwrap(),
            control_origin: format!("http://{}", server.server_addr())
                .try_into()
                .unwrap(),
        };
        let mode = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (audience, mode, calls, stop) =
                (audience.clone(), mode.clone(), calls.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut revision = 1;
                let mut previous_mode = 0;
                while !stop.load(Ordering::SeqCst) {
                    let Some(mut request) = server.recv_timeout(Duration::from_millis(50)).unwrap()
                    else {
                        continue;
                    };
                    assert!(
                        request
                            .headers()
                            .iter()
                            .any(|h| h.field.equiv("Authorization")
                                && h.value.as_str() == format!("Bearer {}", "ab".repeat(32)))
                    );
                    let mut body = Vec::new();
                    request.as_reader().read_to_end(&mut body).unwrap();
                    let input: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(input["audience"], serde_json::to_value(&audience).unwrap());
                    let sequence = calls.fetch_add(1, Ordering::SeqCst) as u64 + 100;
                    let mode = mode.load(Ordering::SeqCst);
                    if mode != previous_mode {
                        revision += 1;
                        previous_mode = mode;
                    }
                    let (status, body) = if mode == 3 {
                        (503, serde_json::json!({}))
                    } else if request.url().ends_with("/service") {
                        let mut value = service(&audience, sequence, mode != 2);
                        value.lease.revocation_generation = Counter(revision);
                        (200, serde_json::to_value(value).unwrap())
                    } else {
                        assert!(request.url().ends_with("/introspect"));
                        assert_eq!(input["grant"], "cd".repeat(32));
                        if mode == 1 {
                            (403, serde_json::json!({}))
                        } else {
                            let mut value = account(&audience, sequence);
                            value.lease.revocation_generation = Counter(revision);
                            (200, serde_json::to_value(value).unwrap())
                        }
                    };
                    request
                        .respond(
                            Response::from_string(body.to_string())
                                .with_status_code(status)
                                .with_header(
                                    Header::from_bytes("Content-Type", "application/json").unwrap(),
                                ),
                        )
                        .unwrap();
                }
            })
        };
        Self {
            audience,
            mode,
            calls,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn cloud(f: &mut Fixture, provider: &Provider) -> Arc<alt_server::cloud::Runtime> {
    f.client()
        .batch_execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .unwrap();
    f.spec.deployment_mode = DeploymentMode::Cloud;
    let runtime = Arc::new(
        alt_server::cloud::Runtime::new(provider.audience.clone(), secret(&"ab".repeat(32)))
            .unwrap(),
    );
    f.store = Store::initialize_cloud(
        f.client(),
        ByteStore::open(&f.root.path().join("bytes")).unwrap(),
        f.spec.clone(),
        SigningKey::from_bytes(&[f.key_seed; 32]),
        limits(),
        &provider.audience,
        runtime.key_digest(),
    )
    .unwrap();
    f.token = format!("alt1_{}", "ef".repeat(32));
    let digest = alt_server_store::cloud_credential_digest(&f.token).unwrap();
    let sealed =
        alt_cloud_client::GrantSealer::new(provider.audience.clone(), &secret(&"ab".repeat(32)))
            .unwrap()
            .seal(&secret(&"cd".repeat(32)), &digest)
            .unwrap();
    f.store
        .accept_cloud_login(
            &f.token,
            &sealed,
            &account(&provider.audience, 1),
            now() + 3_600_000,
            false,
        )
        .unwrap();
    f.credential = f.store.authenticate(&f.token).unwrap();
    f.store
        .refresh_cloud_service(&service(&provider.audience, 2, true))
        .unwrap();
    f.store
        .create_repository(&f.credential, "repository", 100_000_000)
        .unwrap();
    f.store
        .set_grants(&f.credential, "repository", "owner", true, true, &grants())
        .unwrap();
    runtime
}
struct Process {
    child: Child,
    url: String,
    log: PathBuf,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Process {
    fn start(f: &Fixture, provider: &Provider, library: &Path, selected: &Path) -> Self {
        let root = f.root.path();
        let config_path = root.join("server.json");
        let runtime_key = root.join("runtime.key");
        let receipt_key = root.join("receipt.key");
        alt_server::operator::create_private(&runtime_key, "ab".repeat(32).as_bytes()).unwrap();
        alt_server::operator::create_private(&receipt_key, &[f.key_seed; 32]).unwrap();
        let mut config: alt_server::operator::Config =
            serde_json::from_str(include_str!("../../../../example-config.json")).unwrap();
        config.instance = f.spec.clone();
        config.cloud = Some(alt_server::operator::CloudConfig {
            audience: provider.audience.clone(),
            runtime_credential_file: runtime_key,
            provision_digest: None,
        });
        config.browser = Some(alt_server::operator::BrowserConfig {
            public_origin: provider.audience.origin.as_str().into(),
            assets_directory: root.join("assets"),
            session_ttl_ms: 60_000,
        });
        config.database_environment = "ALT_PORTABLE_CLOUD_DATABASE".into();
        config.bytes_directory = root.join("bytes");
        config.receipt_key_file = receipt_key;
        config.listen = "127.0.0.1:0".parse().unwrap();
        config.limits = limits();
        config.tasks = task_limits();
        config.portable = Some(bounds());
        config.validate().unwrap();
        // Explicit fixture volume binding; initialization is covered separately.
        let marker = serde_json::json!({"version":1,"installation_id":f.spec.installation_id,"workspace_id":f.spec.workspace_id,"deployment_mode":f.spec.deployment_mode,"receipt_key_id":alt_hash::blake3::hash(SigningKey::from_bytes(&[f.key_seed;32]).verifying_key().as_bytes()).to_hex().to_string()});
        alt_server::operator::create_private(
            &root.join("bytes/instance.json"),
            &serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        assert!(f.admin_dsn.contains("dbname=postgres"));
        let dsn = f
            .admin_dsn
            .replace("dbname=postgres", &format!("dbname={}", f.database));
        let log = root.join("server.log");
        let child = Command::new(env!("CARGO_BIN_EXE_alt-server"))
            .args(["--config", config_path.to_str().unwrap(), "serve"])
            .env("ALT_PORTABLE_CLOUD_DATABASE", dsn)
            .env("LD_PRELOAD", library)
            .env("ALT_PORTABLE_PAUSE_FILE", selected)
            .stdout(Stdio::piped())
            .stderr(Stdio::from(std::fs::File::create(&log).unwrap()))
            .spawn()
            .unwrap();
        let mut process = Self {
            child,
            url: String::new(),
            log,
        };
        let stdout = process.child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(stdout).read_line(&mut line).unwrap();
            let _ = tx.send(line);
        });
        let line = rx.recv_timeout(Duration::from_secs(15)).unwrap();
        reader.join().unwrap();
        let started: serde_json::Value = serde_json::from_str(&line).unwrap();
        process.url = format!("http://{}", started["address"].as_str().unwrap());
        process
    }
    fn wait_stopped(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let status = std::fs::read_to_string(format!("/proc/{}/status", self.child.id()))
                .unwrap_or_default();
            if status
                .lines()
                .any(|l| l.starts_with("State:") && l.split_whitespace().nth(1) == Some("T"))
            {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "server did not pause on selected content: {}",
                std::fs::read_to_string(&self.log).unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn resume(&self) {
        // SAFETY: this is the live process owned by this Child handle.
        assert_eq!(
            unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGCONT) },
            0
        );
    }
}
fn interposer(root: &Path) -> PathBuf {
    let source = root.join("pause.c");
    let library = root.join("pause.so");
    std::fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static _Atomic int fired = 0;
static void pause_selected(int fd, ssize_t result) {
    const char *selected = getenv("ALT_PORTABLE_PAUSE_FILE");
    if (selected && result > 0 && !atomic_load(&fired)) {
        char link[64], path[4096];
        snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
        ssize_t n = readlink(link, path, sizeof(path)-1);
        if (n >= 0) {
            path[n] = 0;
            if (!strcmp(path, selected) && !atomic_exchange(&fired, 1)) raise(SIGSTOP);
        }
    }
}
ssize_t read(int fd, void *buffer, size_t size) {
    ssize_t (*original)(int, void *, size_t) = dlsym(RTLD_NEXT, "read");
    ssize_t result = original(fd, buffer, size);
    pause_selected(fd, result);
    return result;
}
ssize_t pread(int fd, void *buffer, size_t size, off_t offset) {
    ssize_t (*original)(int, void *, size_t, off_t) = dlsym(RTLD_NEXT, "pread");
    ssize_t result = original(fd, buffer, size, offset);
    pause_selected(fd, result);
    return result;
}
ssize_t pread64(int fd, void *buffer, size_t size, off64_t offset) {
    ssize_t (*original)(int, void *, size_t, off64_t) = dlsym(RTLD_NEXT, "pread64");
    ssize_t result = original(fd, buffer, size, offset);
    pause_selected(fd, result);
    return result;
}
"#,
    )
    .unwrap();
    let compiled = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2", "-o"])
        .arg(&library)
        .arg(&source)
        .arg("-ldl")
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    library
}
fn post(url: String, token: String, body: Vec<u8>) -> (u16, serde_json::Value) {
    let request = ureq::post(&url)
        .timeout(Duration::from_secs(45))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json");
    let response = match request.send_bytes(&body) {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("HTTP request: {e}"),
    };
    (
        response.status(),
        serde_json::from_reader(response.into_reader()).unwrap(),
    )
}
fn expire(f: &Fixture) {
    let mut client = f.client();
    client.batch_execute("SET statement_timeout='2s'").unwrap();
    let mut tx = client.transaction().unwrap();
    let expired = now().to_string();
    assert_eq!(tx.execute("UPDATE alt_cloud_credentials SET claims=jsonb_set(claims,'{lease,expires_at_unix_ms}',to_jsonb($1::text))", &[&expired]).unwrap(),1);
    assert_eq!(tx.execute("UPDATE alt_cloud_binding SET service=jsonb_set(service,'{lease,expires_at_unix_ms}',to_jsonb($1::text))", &[&expired]).unwrap(),1);
    tx.commit().unwrap();
}

#[test]
#[ignore = "requires isolated PostgreSQL, Linux read interposer and actual HTTP server processes; Cloud lease expiry during semantic validation"]
fn portable_cloud_validation_refreshes_current_authority_before_completion() {
    let scratch = tempfile::tempdir().unwrap();
    let library = interposer(scratch.path());
    let source = Fixture::new();
    let source_server = Running::start(&source);
    let a = client(&source_server, &source.token);
    let payload = Payload::new("cloud-validation-source", None);
    publish(&a, &payload);
    let package = bounds()
        .verification(limits(), task_limits())
        .unwrap()
        .package;
    let source_key = SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes();
    let export_key = RequestKey {
        scope: payload.publication.key.scope.clone(),
        actor_id: "owner".into(),
        request_id: "source-export".into(),
    };
    let ExportState::Available { envelope, .. } = a
        .portable_export(&export_key, ExportCommand::Prepare, &source_key, package)
        .unwrap()
    else {
        panic!("source export")
    };
    let mut contents = std::collections::BTreeMap::new();
    for blob in &envelope.catalog.blobs {
        let mut bytes = Vec::new();
        a.portable_download(&export_key.scope, &export_key.request_id, blob, &mut bytes)
            .unwrap();
        contents.insert(blob.blake3.clone(), bytes);
    }
    let selected = &payload
        .manifest
        .entries
        .iter()
        .find(|e| e.kind == ContentKind::Commit)
        .unwrap()
        .blake3;
    // modes: renew, revoke account, suspend paid writes, provider unavailable.
    for importing in [false, true] {
        for mode in [0, 1, 2, 3] {
            let mut f = Fixture::new_with_identity("cloud-installation", 8);
            let provider = Provider::new(&f);
            let runtime = cloud(&mut f, &provider);
            let assets = f.root.path().join("assets");
            std::fs::create_dir(&assets).unwrap();
            std::fs::write(
                assets.join("index.html"),
                "<!doctype html><title>Alt</title>",
            )
            .unwrap();
            let web = alt_server::WebOptions {
                origin: alt_browser_auth::PublicOrigin::parse(provider.audience.origin.as_str())
                    .unwrap(),
                assets: alt_server::WebAssets::load(&assets).unwrap(),
                session_ttl_ms: 60_000,
            };
            let server = Running::start_with_cloud(&f, Some(web), Some(runtime));
            let b = client(&server, &f.token);
            let intent = ImportIntent {
                version: 1,
                key: RequestKey {
                    scope: b.repository("repository").unwrap().scope,
                    actor_id: "owner".into(),
                    request_id: "cloud-import".into(),
                },
                target_public_key: f.store.receipt_public_key(),
                source: export_key.scope.clone(),
                source_public_key: source.store.receipt_public_key(),
                catalog_digest: envelope.catalog.digest(package).unwrap(),
                mappings: envelope
                    .catalog
                    .identities
                    .iter()
                    .map(|i| IdentityMapping {
                        installation_id: i.installation_id.clone(),
                        actor_id: i.actor_id.clone(),
                        target_actor: None,
                    })
                    .collect(),
            };
            if importing {
                b.portable_import_begin(&intent, &envelope, package)
                    .unwrap();
                for blob in &envelope.catalog.blobs {
                    b.portable_upload(&intent.key, blob, &mut &contents[&blob.blake3][..])
                        .unwrap();
                }
            } else {
                let mut cloud_payload = Payload::new("cloud-validation-source", None);
                cloud_payload.publication.key.scope = intent.key.scope.clone();
                publish(&b, &cloud_payload);
            }
            drop(server);
            let selected_path = f
                .root
                .path()
                .join("bytes/cloud-installation/repository")
                .join(selected);
            assert!(selected_path.is_file());
            let mut process = Process::start(&f, &provider, &library, &selected_path);
            let path = if importing {
                "/v1/repositories/repository/epochs/1/portable/imports/owner/cloud-import/activate"
            } else {
                "/v1/repositories/repository/epochs/1/portable/exports/cloud-export"
            };
            let url = format!("{}{path}", process.url);
            let token = f.token.clone();
            let body = if importing {
                serde_json::to_vec(&intent).unwrap()
            } else {
                vec![]
            };
            let request = std::thread::spawn(move || post(url, token, body));
            process.wait_stopped();
            let calls = provider.calls.load(Ordering::SeqCst);
            provider.mode.store(mode, Ordering::SeqCst);
            expire(&f);
            process.resume();
            let (status, body) = request.join().unwrap();
            let allowed = mode == 0 || (!importing && mode == 2);
            assert_eq!(
                status == 200,
                allowed,
                "import={importing},mode={mode},HTTP {status}: {body}"
            );
            assert!(
                provider.calls.load(Ordering::SeqCst) >= calls + 2,
                "expired account and service must both be refreshed"
            );
            let mut db = f.client();
            if importing {
                let state: String = db
                    .query_one(
                        "SELECT state FROM alt_portable_imports WHERE import_id='cloud-import'",
                        &[],
                    )
                    .unwrap()
                    .get(0);
                assert_eq!(state, if allowed { "committed" } else { "receiving" });
                let refs: i64 = db
                    .query_one("SELECT count(*) FROM alt_refs", &[])
                    .unwrap()
                    .get(0);
                assert_eq!(refs, if allowed { 1 } else { 0 });
                let requests: i64 = db
                    .query_one("SELECT count(*) FROM alt_requests", &[])
                    .unwrap()
                    .get(0);
                assert_eq!(requests, 0);
            } else {
                let verified:bool=db.query_one("SELECT verified_at IS NOT NULL FROM alt_portable_exports WHERE export_id='cloud-export'",&[]).unwrap().get(0);
                assert_eq!(verified, allowed);
                let oid: String = db
                    .query_one("SELECT oid FROM alt_refs WHERE name='refs/heads/main'", &[])
                    .unwrap()
                    .get(0);
                assert_eq!(oid, payload.commit.to_string());
            }
            if !allowed && mode != 1 {
                provider.mode.store(0, Ordering::SeqCst);
                expire(&f);
                let body = if importing {
                    serde_json::to_vec(&intent).unwrap()
                } else {
                    vec![]
                };
                let (resumed, result) =
                    post(format!("{}{path}", process.url), f.token.clone(), body);
                assert_eq!(resumed, 200, "same intent did not resume: {result}");
                let count: i64 = if importing {
                    db.query_one(
                        "SELECT count(*) FROM alt_portable_imports WHERE state='committed'",
                        &[],
                    )
                    .unwrap()
                    .get(0)
                } else {
                    db.query_one(
                        "SELECT count(*) FROM alt_portable_exports WHERE verified_at IS NOT NULL",
                        &[],
                    )
                    .unwrap()
                    .get(0)
                };
                assert_eq!(count, 1);
            }
            println!(
                "PASS import={importing} provider_mode={mode}: HTTP {status}, durable state checked"
            );
        }
    }
}
