mod body;
mod connection;
mod device;
mod encoding;
mod file_body;
mod handle;
mod observation;
mod pack_body;
use alt_server_telemetry::{Outcome, Phase};
pub(crate) use observation::{scope as trace_scope, spawn_blocking};
pub(crate) fn spawn_observed<T: Send + 'static>(
    phase: Phase,
    work: impl FnOnce() -> alt_server_store::Result<T> + Send + 'static,
) -> tokio::task::JoinHandle<alt_server_store::Result<T>> {
    spawn_blocking(move || alt_server_store::telemetry::observe(phase, work))
}
mod pool;
mod provision;
mod respond;
mod routes;
mod web;
use crate::{NativeOptions, Output, RequestAction, Route, execute_native, parse_route};
use alt_remote_protocol::{ErrorCode, Failure, Limits, RequestKey, Retry};
use alt_server_store::{ByteStore, Error, InstanceSpec, Store};
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, Method, Response, header},
};
use body::{BodyError, spool};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use respond::{ResponseBody, failure, full, render_output, response, store_failure, unknown};
use std::{
    io::{Seek, SeekFrom},
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::Semaphore};
pub use web::WebOptions;

/// Persisted instance identity is required in every mode. Opening never calls
/// initialization or local bootstrap when configuration/authorization is absent.
#[derive(Clone)]
pub struct StoreFactory {
    pub change_links: Arc<[crate::change_links::ChannelRuntime]>,
    pub cloud: Option<Arc<crate::cloud::Runtime>>,
    pub database: alt_pg::Config,
    pub bytes: ByteStore,
    pub instance: InstanceSpec,
    pub signing_key: alt_crypto::ed25519::SigningKey,
    pub limits: Limits,
    pub connect_timeout: Duration,
    pub statement_timeout: Duration,
}
impl StoreFactory {
    pub fn open(&self) -> alt_server_store::Result<Store> {
        let mut config = self.database.clone();
        config.set_connect_timeout(self.connect_timeout);
        let mut client = config.connect()?;
        let timeout = self.statement_timeout.as_millis();
        if timeout == 0 || timeout > 300_000 {
            return Err(Error::Limit("database statement timeout"));
        }
        // a plan cached while a table is one page scans it sequentially and stays in
        // use as the table grows; once statistics make a plan for each parameter look
        // cheaper, every execution is planned again. both hinge on when analyze runs
        client.batch_execute(&format!(
            "SET statement_timeout = {timeout}; SET lock_timeout = {timeout}; \
             SET enable_seqscan = off; SET plan_cache_mode = force_generic_plan"
        ))?;
        let mut store = Store::open(
            client,
            self.bytes.clone(),
            self.instance.clone(),
            self.signing_key.clone(),
            self.limits,
        )?;
        self.check_mode(&mut store)?;
        Ok(store)
    }
    fn check_mode(&self, store: &mut Store) -> alt_server_store::Result<()> {
        match (&self.cloud, self.instance.deployment_mode) {
            (Some(cloud), alt_remote_protocol::DeploymentMode::Cloud) => cloud.check(store)?,
            (None, alt_remote_protocol::DeploymentMode::SelfHosted) => (),
            _ => return Err(Error::Unauthorized),
        }
        Ok(())
    }
    pub fn authenticate(
        &self,
        store: &mut Store,
        token: &str,
    ) -> alt_server_store::Result<alt_server_store::Credential> {
        if let Some(cloud) = &self.cloud {
            cloud.refresh(store, token)?;
        }
        store.authenticate(token)
    }
    /// authentication and the repository view of a Git request in one transaction
    pub fn git_open(
        &self,
        store: &mut Store,
        token: &str,
        repository: &str,
        filter: alt_server_store::RefFilter<'_>,
    ) -> alt_server_store::Result<(alt_server_store::Credential, alt_server_store::PackView)> {
        if let Some(cloud) = &self.cloud {
            cloud.refresh(store, token)?;
        }
        store.git_open(token, repository, filter)
    }
}
#[derive(Clone)]
pub struct HttpOptions {
    pub connections: usize,
    pub workers: usize,
    pub header_bytes: usize,
    pub headers: usize,
    pub header_timeout: Duration,
    pub body_timeout: Duration,
    pub work_timeout: Duration,
    pub connection_timeout: Duration,
    pub native: NativeOptions,
}
struct App {
    metrics: Arc<alt_server_telemetry::Metrics>,
    compaction: crate::compaction::Compaction,
    factory: pool::Pool,
    options: HttpOptions,
    workers: Arc<Semaphore>,
    browser: Option<WebOptions>,
    passwords: Arc<Semaphore>,
}
pub fn authorization(headers: &HeaderMap) -> Option<String> {
    if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
        return None;
    }
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    if value.len() > 512 {
        return None;
    }
    let token = if let Some(token) = value.strip_prefix("Bearer ") {
        token.to_owned()
    } else {
        let encoded = value.strip_prefix("Basic ")?;
        let decoded = alt_base64::STANDARD.decode(encoded).ok()?;
        let text = std::str::from_utf8(&decoded).ok()?;
        let (username, password) = text.split_once(':')?;
        if username.is_empty() || username.chars().any(char::is_control) {
            return None;
        }
        password.to_owned()
    };
    if token.len() != 69
        || !token.starts_with("alt1_")
        || !token[5..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    Some(token)
}
/// Bounded HTTP/1 transport; TLS is terminated by the packaged reverse proxy.
/// Shutdown stops accepting connections; in-flight writes retain their normal
/// durable result/unknown semantics and all database worker fences.
pub async fn serve(
    listener: TcpListener,
    factory: StoreFactory,
    options: HttpOptions,
    shutdown: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    serve_with_browser(listener, factory, options, None, shutdown).await
}
pub async fn serve_with_browser(
    listener: TcpListener,
    factory: StoreFactory,
    options: HttpOptions,
    browser: Option<WebOptions>,
    shutdown: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    if let Some(portable) = options.native.portable {
        portable
            .verification(factory.limits, options.native.task_limits)
            .map_err(|_| std::io::Error::other("invalid portable limits"))?;
    }
    if let Some(ref browser) = browser {
        browser.validate()?;
    }
    let check_factory = factory.clone();
    let origin = browser.as_ref().map(|b| b.origin.clone());
    spawn_blocking(move || check_factory.open()?.check_browser_binding(origin.as_ref()))
        .await
        .map_err(|_| std::io::Error::other("browser binding check unavailable"))?
        .map_err(|_| {
            std::io::Error::other(
                "the browser origin recorded in the database differs from the configured one",
            )
        })?;
    if options.connections == 0
        || options.workers == 0
        || options.header_bytes < 8192
        || options.headers == 0
        || [
            options.header_timeout,
            options.body_timeout,
            options.work_timeout,
            options.connection_timeout,
        ]
        .contains(&Duration::ZERO)
    {
        return Err(std::io::Error::other("invalid HTTP limits"));
    }
    let mut background = tokio::task::JoinSet::new();
    let connections = Arc::new(Semaphore::new(options.connections));
    let state = Arc::new(App {
        metrics: alt_server_telemetry::Metrics::new(alt_server_telemetry::process_log_losses())
            .map_err(|_| std::io::Error::other("metrics initialization unavailable"))?,
        compaction: crate::compaction::Compaction::start(factory.clone()),
        factory: pool::Pool::new(factory, options.workers),
        workers: Arc::new(Semaphore::new(options.workers)),
        options,
        browser,
        passwords: Arc::new(Semaphore::new(2)),
    });
    if !state.factory.change_links.is_empty() {
        background.spawn(crate::change_links::worker(
            (*state.factory).clone(),
            state.metrics.clone(),
        ));
    }
    let router = routes::router(state.clone());
    tokio::pin!(shutdown);
    loop {
        let (socket, peer) = tokio::select! {
            _ = &mut shutdown => {
                state.factory.shutdown().await?;
                return Ok(());
            },
            accepted = listener.accept() => accepted?,
        };
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            drop(socket);
            continue;
        };
        // File bodies can become ready after the response headers are sent.
        // Do not hold their final small frame until the peer's delayed ACK.
        if socket.set_nodelay(true).is_err() {
            continue;
        }
        let (state, router) = (state.clone(), router.clone());
        tokio::spawn(async move {
            let _permit = permit;
            connection::serve(socket, peer, router, &state.options).await;
        });
    }
}
