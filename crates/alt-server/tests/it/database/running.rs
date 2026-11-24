//! The HTTP service on a loopback port, stopped when dropped.
use super::*;

pub(super) struct Running {
    pub(super) url: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Running {
    pub(super) fn start(f: &Fixture) -> Self {
        Self::start_with_browser(f, None)
    }
    pub(super) fn start_with_browser(f: &Fixture, browser: Option<alt_server::WebOptions>) -> Self {
        Self::start_with_cloud(f, browser, None)
    }
    pub(super) fn start_with_cloud(
        f: &Fixture,
        browser: Option<alt_server::WebOptions>,
        cloud: Option<std::sync::Arc<alt_server::cloud::Runtime>>,
    ) -> Self {
        Self::start_with_links(f, browser, cloud, vec![])
    }
    pub(super) fn start_with_links(
        f: &Fixture,
        browser: Option<alt_server::WebOptions>,
        cloud: Option<std::sync::Arc<alt_server::cloud::Runtime>>,
        links: Vec<alt_server::change_links::ChannelRuntime>,
    ) -> Self {
        Self::start_with_shutdown(f, browser, cloud, links, false)
    }
    pub(super) fn parts(
        f: &Fixture,
        cloud: Option<std::sync::Arc<alt_server::cloud::Runtime>>,
        links: Vec<alt_server::change_links::ChannelRuntime>,
    ) -> (StoreFactory, HttpOptions) {
        let mut database: alt_pg::Config = f.admin_dsn.parse().unwrap();
        database.set_dbname(&f.database);
        let signing_key = SigningKey::from_bytes(&[f.key_seed; 32]);
        let factory = StoreFactory {
            change_links: links.into(),
            cloud,
            database,
            bytes: ByteStore::open(&f.root.path().join("bytes")).unwrap(),
            instance: f.spec.clone(),
            signing_key,
            limits: limits(),
            connect_timeout: Duration::from_secs(3),
            statement_timeout: Duration::from_secs(5),
        };
        let options = HttpOptions {
            connections: 16,
            workers: 4,
            header_bytes: 16384,
            headers: 32,
            header_timeout: Duration::from_secs(3),
            body_timeout: Duration::from_secs(3),
            work_timeout: Duration::from_secs(10),
            connection_timeout: Duration::from_secs(30),
            trusted_proxies: vec!["127.0.0.1".parse().unwrap()],
            connections_each: 16,
            receiving_each: 4,
            receiving_bytes: 1 << 30,
            expiry_interval: Duration::from_secs(3600),
            password_places: 2,
            verifying: Box::leak(Box::new(alt_server::PackMemory::new(4 << 30))),
            native: NativeOptions {
                portable: Some(portable::bounds()),
                task_limits: task_limits(),
                upload_ttl_ms: 60_000,
                worker_lease_ms: 30_000,
                approval_ttl_ms: 60_000,
            },
        };
        (factory, options)
    }
    /// what the service reports when it refuses to start with this browser configuration
    pub(super) fn refusal(f: &Fixture, browser: Option<alt_server::WebOptions>) -> String {
        let (factory, options) = Self::parts(f, None, vec![]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            alt_server::serve_with_browser(listener, factory, options, browser, async {})
                .await
                .unwrap_err()
                .to_string()
        })
    }
    pub(super) fn start_with_shutdown(
        f: &Fixture,
        browser: Option<alt_server::WebOptions>,
        cloud: Option<std::sync::Arc<alt_server::cloud::Runtime>>,
        links: Vec<alt_server::change_links::ChannelRuntime>,
        abort: bool,
    ) -> Self {
        let (factory, options) = Self::parts(f, cloud, links);
        Self::serve(factory, options, browser, abort)
    }
    pub(super) fn serve(
        factory: StoreFactory,
        options: HttpOptions,
        browser: Option<alt_server::WebOptions>,
        abort: bool,
    ) -> Self {
        let (address_tx, address_rx) = std::sync::mpsc::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                address_tx.send(listener.local_addr().unwrap()).unwrap();
                if abort {
                    let serving = tokio::spawn(alt_server::serve_with_browser(
                        listener,
                        factory,
                        options,
                        browser,
                        std::future::pending(),
                    ));
                    let _ = shutdown_rx.await;
                    serving.abort();
                    assert!(serving.await.unwrap_err().is_cancelled());
                } else {
                    alt_server::serve_with_browser(listener, factory, options, browser, async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .unwrap();
                }
            });
        });
        let address = address_rx.recv().unwrap();
        Self {
            url: format!("http://{address}"),
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        }
    }
    pub(super) fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &[u8],
    ) -> ureq::Response {
        let mut request =
            ureq::request(method, &format!("{}{path}", self.url)).timeout(Duration::from_secs(15));
        if method == "POST"
            && (path.ends_with("/requests")
                || path.ends_with("/approve")
                || path.contains("/changes/")
                || path == "/api/v1/change-links/inbox")
        {
            request = request.set("Content-Type", "application/json");
        }
        if let Some(token) = token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        match request.send_bytes(body) {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("HTTP transport failed: {error}"),
        }
    }
    pub(super) fn json(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: &impl alt_json::ToJson,
    ) -> (u16, alt_json::Value) {
        let response = self.send(method, path, Some(token), &alt_json::to_vec(body).unwrap());
        let status = response.status();
        (status, crate::database::json::body(response))
    }
    pub(super) fn metrics(&self, token: &str) -> String {
        let response = self.send("GET", "/v1/instance/metrics", Some(token), b"");
        assert_eq!(response.status(), 200);
        response.into_string().unwrap()
    }
    pub(super) fn wait_metric(&self, token: &str, sample: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let text = self.metrics(token);
            if text.lines().any(|line| line == sample) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "missing metric {sample}: {text}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pub(super) fn command(&self, method: &str, path: &str, token: &str) -> (u16, alt_json::Value) {
        let response = self.send(method, path, Some(token), b"");
        let status = response.status();
        (status, crate::database::json::body(response))
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.thread.take().unwrap().join().unwrap();
    }
}
