mod body;
mod bounded;
mod caller;
mod connection;
mod device;
mod encoding;
mod expiry;
mod file_body;
mod handle;
mod hashers;
mod observation;
mod paced;
mod pack_body;
use alt_server_telemetry::{Outcome, Phase};
pub(crate) use observation::{scope as trace_scope, spawn_blocking, spawn_observed};
mod pool;
mod provision;
mod receiving;
mod respond;
mod routes;
mod serving;
mod sources;
mod web;
mod work;
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
pub use caller::Network;
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
    /// the reverse proxies whose `X-Forwarded-For` is read for the address a
    /// request came from; with none the address is the peer's
    pub trusted_proxies: Vec<Network>,
    /// the connections one address may have in use at once
    pub connections_each: usize,
    /// the bodies one caller may have being received at once
    pub receiving_each: usize,
    /// the bytes of bodies that are being received, or wait for a worker
    pub receiving_bytes: u64,
    /// the memory the packs of pushes are verified in, all at once
    pub verifying: &'static alt_pack_segment::Memory,
    /// where the bases of deltas that memory has no room for are written, and what
    /// the packs may take there; among the host's temporary files where not told
    pub base_files: Option<&'static alt_pack_segment::BaseFiles>,
    /// the room on disk for the responses to fetches that are files until they are sent
    pub sending: Arc<alt_shares::Shares>,
    /// how often requests whose time to upload ran out are ended
    pub expiry_interval: Duration,
    /// the passwords that are checked at once: each takes the memory of one hash
    pub password_places: usize,
    pub native: NativeOptions,
}
struct App {
    metrics: Arc<alt_server_telemetry::Metrics>,
    compaction: crate::compaction::Compaction,
    factory: pool::Pool,
    options: HttpOptions,
    workers: Arc<Semaphore>,
    receiving: Arc<receiving::Receiving>,
    sources: Arc<sources::Sources>,
    browser: Option<WebOptions>,
    passwords: hashers::Hashers,
    reviews: Arc<crate::web_review::Reviews>,
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
    if !options.bounded() {
        return Err(std::io::Error::other("invalid HTTP limits"));
    }
    let serves = factory.clone();
    let mut alone = spawn_blocking(move || serving::alone(serves))
        .await
        .map_err(|_| std::io::Error::other("serving session unavailable"))??;
    if options.trusted_proxies.is_empty() {
        // in the log like everything the instance says: a line of its own
        // kind would be no line of the log to whoever reads it
        let log = alt_server_telemetry::BackgroundLog::current();
        log.failed("configuration", "", &caller::UNNAMED);
    }
    let mut background = tokio::task::JoinSet::new();
    let connections = Arc::new(Semaphore::new(options.connections));
    let state = Arc::new(App {
        metrics: alt_server_telemetry::Metrics::new(alt_server_telemetry::process_log_losses())
            .map_err(|_| std::io::Error::other("metrics initialization unavailable"))?,
        compaction: crate::compaction::Compaction::start(factory.clone()),
        factory: pool::Pool::new(factory, options.workers),
        workers: Arc::new(Semaphore::new(options.workers)),
        receiving: receiving::Receiving::new(options.receiving_each, options.receiving_bytes),
        sources: sources::Sources::new(options.connections_each, &options.trusted_proxies),
        passwords: hashers::Hashers::new(options.password_places),
        reviews: Arc::new(crate::web_review::Reviews::of(options.workers)),
        options,
        browser,
    });
    if !state.factory.change_links.is_empty() {
        background.spawn(crate::change_links::worker(
            (*state.factory).clone(),
            state.metrics.clone(),
        ));
    }
    background.spawn(expiry::run(state.clone()));
    let router = routes::router(state.clone());
    tokio::pin!(shutdown);
    loop {
        let (socket, peer) = tokio::select! {
            _ = &mut shutdown => {
                state.factory.shutdown().await?;
                return alone.ended().await;
            },
            _ = &mut alone.lost => {
                state.factory.shutdown().await?;
                return Err(std::io::Error::other(alt_server_store::SERVED));
            },
            accepted = listener.accept() => accepted?,
        };
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            drop(socket);
            continue;
        };
        // an address that has as many as it may is not listened to
        let Ok(place) = state.sources.connected(peer) else {
            continue;
        };
        // File bodies can become ready after the response headers are sent.
        // Do not hold their final small frame until the peer's delayed ACK.
        if socket.set_nodelay(true).is_err() {
            continue;
        }
        let (state, router) = (state.clone(), router.clone());
        tokio::spawn(async move {
            let _permit = (permit, place);
            connection::serve(socket, peer, router, &state).await;
        });
    }
}
