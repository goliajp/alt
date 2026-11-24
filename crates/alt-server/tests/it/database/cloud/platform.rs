//! The platform a Cloud instance asks, as far as these tests need it, and an
//! instance bound to it.
use super::*;
use std::time::Instant;

pub(in crate::database) struct Platform {
    pub audience: Audience,
    pub claims: AccountAuthorization,
    /// 0 answers, 1 is out, 2 refuses what it is asked to look at, 3 fails exchanges,
    /// 4 is out and says so half a second after it is asked
    pub mode: Arc<AtomicUsize>,
    pub calls: Arc<AtomicUsize>,
    /// when each call came, and when what it was told was about to be sent
    pub told: Arc<std::sync::Mutex<Vec<(Instant, Instant)>>>,
    pub challenge: Arc<std::sync::Mutex<String>>,
    pub exchanges: Arc<AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: std::thread::JoinHandle<()>,
}

impl Platform {
    pub fn start(f: &Fixture) -> Self {
        let provider = Server::http("127.0.0.1:0").unwrap();
        let audience = Audience {
            organization_id: "organization".to_string().try_into().unwrap(),
            workspace_id: f.spec.workspace_id.clone().try_into().unwrap(),
            installation_id: f.spec.installation_id.clone().try_into().unwrap(),
            product: Product::Alt,
            generation: Counter(1),
            origin: "https://alt.example.test".to_string().try_into().unwrap(),
            control_origin: format!("http://{}", provider.server_addr())
                .try_into()
                .unwrap(),
        };
        let claims = AccountAuthorization {
            lease: lease(&audience, 1),
            account_id: "owner".to_string().try_into().unwrap(),
            session_id: "platform-session".to_string().try_into().unwrap(),
            membership_id: "member".to_string().try_into().unwrap(),
            display_name: "Shared account".into(),
            capabilities: vec![Capability::UseProduct, Capability::ManageInstance],
        };
        let mode = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let told = Arc::new(std::sync::Mutex::new(Vec::new()));
        let challenge = Arc::new(std::sync::Mutex::new(String::new()));
        let exchanges = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = {
            let challenge = challenge.clone();
            let (exchanges, told) = (exchanges.clone(), told.clone());
            let (mode, calls, stop, audience, mut claims) = (
                mode.clone(),
                calls.clone(),
                stop.clone(),
                audience.clone(),
                claims.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Some(mut request) =
                        provider.recv_timeout(Duration::from_millis(50)).unwrap()
                    else {
                        continue;
                    };
                    let came = Instant::now();
                    assert!(
                        request
                            .headers()
                            .iter()
                            .any(|h| h.field.equiv("Authorization")
                                && h.value.as_str() == format!("Bearer {}", "ab".repeat(32)))
                    );
                    let mut bytes = Vec::new();
                    request.as_reader().read_to_end(&mut bytes).unwrap();
                    let input: alt_json::Value = alt_json::from_slice(&bytes).unwrap();
                    assert_eq!(
                        input["audience"],
                        alt_json_serde::to_value(&&audience).unwrap()
                    );
                    let sequence = calls.fetch_add(1, Ordering::SeqCst) as u64 + 2;
                    let (status, body) = match mode.load(Ordering::SeqCst) {
                        3 if request.url().ends_with("/exchange") => {
                            exchanges.fetch_add(1, Ordering::SeqCst);
                            (503, alt_json::json!({}))
                        }
                        1 => (503, alt_json::json!({})),
                        4 => {
                            std::thread::sleep(Duration::from_millis(500));
                            (503, alt_json::json!({}))
                        }
                        _ if request.url().ends_with("/exchange") => {
                            exchanges.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(input["ticket"], "ff".repeat(32));
                            let verifier = secret(input["verifier"].as_str().unwrap());
                            assert_eq!(
                                golia_platform_contract::pkce_challenge(&verifier),
                                *challenge.lock().unwrap()
                            );
                            claims.lease = lease(&audience, sequence);
                            (
                                200,
                                alt_json::json!({"version":1,"grant":"cd".repeat(32),"account":alt_json_serde::Serde(&claims)}),
                            )
                        }
                        2 if request.url().ends_with("/introspect") => (403, alt_json::json!({})),
                        _ if request.url().ends_with("/service") => (
                            200,
                            alt_json_serde::to_value(&AltServiceAuthorization {
                                lease: lease(&audience, sequence),
                                writes_enabled: false,
                                limits: AltLimits {
                                    repositories: Counter(2),
                                    stored_bytes: Counter(100_000),
                                    monthly_ingress_bytes: Counter(100_000),
                                },
                            })
                            .unwrap(),
                        ),
                        _ => {
                            assert_eq!(input["grant"], "cd".repeat(32));
                            claims.lease = lease(&audience, sequence);
                            (200, alt_json_serde::to_value(&&claims).unwrap())
                        }
                    };
                    told.lock().unwrap().push((came, Instant::now()));
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
            claims,
            mode,
            calls,
            told,
            challenge,
            exchanges,
            stop,
            worker,
        }
    }
    pub fn end(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.join().unwrap();
    }
}

pub(in crate::database) struct Instance {
    pub server: Running,
    pub runtime: Arc<alt_server::cloud::Runtime>,
    pub origin: alt_browser_auth::PublicOrigin,
    /// of the account that signed in before the server was started
    pub token: String,
}

/// Makes the instance of `f` a Cloud one bound to `platform`, with an account
/// signed in, and serves it after `adjust` changed its bounds.
pub(in crate::database) fn instance(
    f: &mut Fixture,
    platform: &Platform,
    adjust: impl FnOnce(&mut HttpOptions),
) -> Instance {
    let audience = platform.audience.clone();
    let mut claims = platform.claims.clone();
    f.client()
        .batch_execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .unwrap();
    f.spec.deployment_mode = DeploymentMode::Cloud;
    let runtime = Arc::new(
        alt_server::cloud::Runtime::new(audience.clone(), secret(&"ab".repeat(32))).unwrap(),
    );
    f.store = Store::initialize_cloud(
        f.client(),
        ByteStore::open(&f.root.path().join("bytes")).unwrap(),
        f.spec.clone(),
        SigningKey::from_bytes(&[7; 32]),
        limits(),
        &audience,
        runtime.key_digest(),
    )
    .unwrap();
    let token = format!("alt1_{}", "ef".repeat(32));
    let digest = alt_server_store::cloud_credential_digest(&token).unwrap();
    let sealed = alt_cloud_client::GrantSealer::new(audience.clone(), &secret(&"ab".repeat(32)))
        .unwrap()
        .seal(&secret(&"cd".repeat(32)), &digest)
        .unwrap();
    claims.lease.expires_at_unix_ms = Counter(time() + 5000);
    f.store
        .accept_cloud_login(&token, &sealed, &claims, time() + 3_600_000, true)
        .unwrap();
    let assets = f.root.path().join("assets");
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>alt</title>",
    )
    .unwrap();
    let origin = alt_browser_auth::PublicOrigin::parse(audience.origin.as_str()).unwrap();
    let (factory, mut options) = Running::parts(f, Some(runtime.clone()), vec![]);
    adjust(&mut options);
    let web = alt_server::WebOptions {
        origin: origin.clone(),
        assets: alt_server::WebAssets::load(&assets).unwrap(),
        session_ttl_ms: 60_000,
    };
    let server = Running::serve(factory, options, Some(web), false);
    Instance {
        server,
        runtime,
        origin,
        token,
    }
}
