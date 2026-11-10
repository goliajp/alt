//! Bounded process metrics and explicit, redacted request observations.
//!
//! A transport ending says nothing about a database transaction. Work may
//! finish after its HTTP waiter disappears; both observations retain one trace.
mod body;
mod snapshot;
pub use body::{ObservedBody, ResponseObservation};
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    TextEncoder,
};
use rand_core::{OsRng, RngCore};
pub use snapshot::Snapshot;
use std::{
    cell::RefCell,
    io,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
use tracing_appender::non_blocking::{ErrorCounter, NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::prelude::*;

macro_rules! labels {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant),+ }
        impl $name { pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $text),+ } } }
    }
}
labels!(Route { Health => "health", Capabilities => "capabilities", Instance => "instance", Metrics => "metrics", Repository => "repository", Request => "request", Upload => "upload", Publication => "publication", GitAdvertise => "git_advertise", GitUpload => "git_upload", GitReceive => "git_receive", Portable => "portable", ChangeLink => "change_link", Delegation => "delegation", Browser => "browser", Asset => "asset", Device => "device", Provision => "provision", Unknown => "unknown" });
labels!(Method { Get => "GET", Post => "POST", Put => "PUT", Delete => "DELETE", Head => "HEAD", Other => "other" });
impl Method {
    pub fn from_http(method: &str) -> Self {
        match method {
            "GET" => Self::Get,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "DELETE" => Self::Delete,
            "HEAD" => Self::Head,
            _ => Self::Other,
        }
    }
}
labels!(Actor { Unauthenticated => "unauthenticated", Authenticated => "authenticated", Human => "human", Agent => "agent", Service => "service" });
labels!(Outcome { Ok => "ok", Denied => "denied", Invalid => "invalid", Conflict => "conflict", Precondition => "precondition", Limit => "limit", Integrity => "integrity", Unavailable => "unavailable", Unknown => "unknown", Interrupted => "interrupted" });
labels!(Transport { Complete => "complete", Failed => "failed", Dropped => "dropped" });
labels!(Phase { Authority => "authority", Upload => "upload", Decode => "decode", Validate => "validate", Persist => "persist", Commit => "commit", Execute => "execute", Queue => "queue", Dispatch => "dispatch", Acknowledge => "acknowledge", GarbageCollect => "garbage_collect" });

/// Identifiers only. Never pass URLs, paths, credentials or descriptive text.
/// Size/alphabet restrictions prevent line injection and unbounded log records.
#[derive(Clone, Debug)]
pub struct Id(String);
impl Id {
    pub fn new(value: &str) -> Option<Self> {
        (!value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
        .then(|| Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

static LOG_LOSSES: OnceLock<ErrorCounter> = OnceLock::new();

pub fn process_log_losses() -> Option<ErrorCounter> {
    LOG_LOSSES.get().cloned()
}

pub struct Logging {
    _guard: WorkerGuard,
    dropped: ErrorCounter,
}
impl Logging {
    pub fn dropped(&self) -> ErrorCounter {
        self.dropped.clone()
    }
}
/// Install once in the server executable. Only this target is enabled; changing
/// RUST_LOG cannot accidentally expose database or HTTP library debug payloads.
pub fn install_logging(
    writer: impl io::Write + Send + 'static,
    buffered_lines: usize,
) -> Result<Logging, Box<dyn std::error::Error + Send + Sync>> {
    if buffered_lines == 0 {
        return Err("log capacity must be positive".into());
    }
    let (writer, guard) = NonBlockingBuilder::default()
        .buffered_lines_limit(buffered_lines)
        .lossy(true)
        .finish(writer);
    let dropped = writer.error_counter();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(false)
            .with_span_list(false)
            .with_writer(writer)
            .with_filter(Targets::new().with_target("alt_server_telemetry", tracing::Level::INFO)),
    );
    subscriber.try_init()?;
    let _ = LOG_LOSSES.set(dropped.clone());
    Ok(Logging {
        _guard: guard,
        dropped,
    })
}

pub struct Metrics {
    registry: Registry,
    requests: IntCounterVec,
    duration: HistogramVec,
    bytes: IntCounterVec,
    work_bytes: IntCounterVec,
    phases: HistogramVec,
    outcomes: IntCounterVec,
    inflight: IntGauge,
    dropped: IntCounter,
    observed_dropped: AtomicU64,
    dropped_source: Option<ErrorCounter>,
    boot: String,
    next: AtomicU64,
}
impl Metrics {
    pub fn new(
        dropped_source: Option<ErrorCounter>,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error + Send + Sync>> {
        let registry = Registry::new();
        let requests = IntCounterVec::new(
            Opts::new(
                "alt_http_responses_total",
                "HTTP response bodies by terminal transport observation; not transaction outcomes",
            ),
            &["route", "method", "status_class", "transport"],
        )?;
        let duration = HistogramVec::new(
            HistogramOpts::new(
                "alt_http_response_seconds",
                "Time until response body ends or is dropped",
            )
            .buckets(buckets()),
            &["route", "transport"],
        )?;
        let bytes = IntCounterVec::new(
            Opts::new(
                "alt_http_response_bytes_total",
                "Body bytes yielded to HTTP; not confirmed remote receipt",
            ),
            &["route"],
        )?;
        let work_bytes = IntCounterVec::new(
            Opts::new(
                "alt_work_bytes_total",
                "Bytes observed by explicit work phases, including interrupted attempts",
            ),
            &["phase"],
        )?;
        let phases = HistogramVec::new(
            HistogramOpts::new(
                "alt_operation_phase_seconds",
                "Explicit work phase wall duration, including interrupted attempts",
            )
            .buckets(buckets()),
            &["phase", "outcome"],
        )?;
        let outcomes = IntCounterVec::new(
            Opts::new(
                "alt_operation_outcomes_total",
                "Explicit work outcomes; phase execution success alone is not transaction commit",
            ),
            &["phase", "outcome"],
        )?;
        let inflight = IntGauge::new(
            "alt_http_inflight",
            "Requests with no terminal body observation yet",
        )?;
        let dropped = IntCounter::new(
            "alt_log_dropped_total",
            "Log records dropped by the bounded process writer",
        )?;
        for collector in [
            Box::new(requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(duration.clone()),
            Box::new(bytes.clone()),
            Box::new(work_bytes.clone()),
            Box::new(phases.clone()),
            Box::new(outcomes.clone()),
            Box::new(inflight.clone()),
            Box::new(dropped.clone()),
        ] {
            registry.register(collector)?;
        }
        let mut boot = [0u8; 16];
        OsRng
            .try_fill_bytes(&mut boot)
            .map_err(|_| io::Error::other("trace identity unavailable"))?;
        Ok(Arc::new(Self {
            registry,
            requests,
            duration,
            bytes,
            work_bytes,
            phases,
            outcomes,
            inflight,
            dropped,
            observed_dropped: AtomicU64::new(0),
            dropped_source,
            boot: boot.iter().map(|b| format!("{b:02x}")).collect(),
            next: AtomicU64::new(1),
        }))
    }
    fn observe_log_losses(&self) {
        if let Some(source) = &self.dropped_source {
            let current = source.dropped_lines() as u64;
            let prior = self.observed_dropped.fetch_max(current, Ordering::Relaxed);
            self.dropped.inc_by(current.saturating_sub(prior));
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>, prometheus::Error> {
        self.observe_log_losses();
        let mut bytes = Vec::new();
        TextEncoder::new().encode(&self.registry.gather(), &mut bytes)?;
        Ok(bytes)
    }
}
fn buckets() -> Vec<f64> {
    vec![
        0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 120.0, 600.0,
    ]
}

#[derive(Clone)]
pub struct Trace(Arc<TraceInner>);
struct TraceInner {
    id: String,
    instance: Id,
    route: Route,
    method: Method,
    http: bool,
    start: Instant,
    metrics: Arc<Metrics>,
    dispatch: tracing::Dispatch,
    context: Mutex<Context>,
    ended: AtomicBool,
}
struct Context {
    repository: Option<Id>,
    request: Option<Id>,
    transaction: Option<Id>,
    event: Option<Id>,
    actor: Actor,
}
thread_local! { static CURRENT: RefCell<Option<Trace>> = const { RefCell::new(None) }; }
impl Trace {
    pub fn start(metrics: Arc<Metrics>, instance: Id, route: Route, method: Method) -> Self {
        Self::new(metrics, instance, route, method, true)
    }
    /// A background attempt has work observations but no HTTP request/body.
    pub fn background(metrics: Arc<Metrics>, instance: Id, route: Route) -> Self {
        Self::new(metrics, instance, route, Method::Other, false)
    }
    fn new(metrics: Arc<Metrics>, instance: Id, route: Route, method: Method, http: bool) -> Self {
        let sequence = metrics.next.fetch_add(1, Ordering::Relaxed);
        assert_ne!(sequence, u64::MAX, "trace sequence exhausted");
        if http {
            metrics.inflight.inc();
        }
        let trace = Self(Arc::new(TraceInner {
            id: format!("{}-{sequence:016x}", metrics.boot),
            instance,
            route,
            method,
            http,
            start: Instant::now(),
            metrics,
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            ended: AtomicBool::new(false),
            context: Mutex::new(Context {
                repository: None,
                request: None,
                transaction: None,
                event: None,
                actor: Actor::Unauthenticated,
            }),
        }));
        trace.event(
            if http {
                "request.started"
            } else {
                "background.started"
            },
            "none",
            "none",
            0,
            0,
            0.0,
        );
        trace
    }
    pub fn id(&self) -> &str {
        &self.0.id
    }
    pub fn current() -> Option<Self> {
        CURRENT.with(|slot| slot.borrow().clone())
    }
    pub fn in_scope<T>(&self, f: impl FnOnce() -> T) -> T {
        struct Restore(Option<Trace>);
        impl Drop for Restore {
            fn drop(&mut self) {
                CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
            }
        }
        let _restore = Restore(CURRENT.with(|slot| slot.replace(Some(self.clone()))));
        tracing::dispatcher::with_default(&self.0.dispatch, f)
    }
    pub fn identity(&self, repository: Option<Id>, request: Option<Id>, transaction: Option<Id>) {
        let mut context = self.0.context.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(value) = repository {
            context.repository = Some(value);
        }
        if let Some(value) = request {
            context.request = Some(value);
        }
        if let Some(value) = transaction {
            context.transaction = Some(value);
        }
    }
    /// The durable signed event identity, distinct from a publication request.
    pub fn event_identity(&self, event: Id) {
        self.0
            .context
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .event = Some(event);
    }
    pub fn actor(&self, actor: Actor) {
        self.0
            .context
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .actor = actor;
    }
    pub fn headers(&self, status: u16) {
        self.event(
            "response.headers",
            "none",
            "none",
            status,
            0,
            self.0.start.elapsed().as_secs_f64(),
        );
    }
    pub fn transport(&self, status: u16, bytes: u64, terminal: Transport) {
        if !self.0.http || self.0.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        let elapsed = self.0.start.elapsed().as_secs_f64();
        let class = match status {
            100..=199 => "1xx",
            200..=299 => "2xx",
            300..=399 => "3xx",
            400..=499 => "4xx",
            500..=599 => "5xx",
            _ => "none",
        };
        let metrics = &self.0.metrics;
        metrics
            .requests
            .with_label_values(&[
                self.0.route.as_str(),
                self.0.method.as_str(),
                class,
                terminal.as_str(),
            ])
            .inc();
        metrics
            .duration
            .with_label_values(&[self.0.route.as_str(), terminal.as_str()])
            .observe(elapsed);
        metrics
            .bytes
            .with_label_values(&[self.0.route.as_str()])
            .inc_by(bytes);
        metrics.inflight.dec();
        self.event(
            "response.ended",
            "none",
            terminal.as_str(),
            status,
            bytes,
            elapsed,
        );
    }
    fn event(
        &self,
        event: &'static str,
        phase: &'static str,
        outcome: &'static str,
        status: u16,
        bytes: u64,
        elapsed: f64,
    ) {
        let c = self.0.context.lock().unwrap_or_else(|p| p.into_inner());
        tracing::dispatcher::with_default(&self.0.dispatch, || {
            tracing::info!(target: "alt_server_telemetry", event, origin = if self.0.http { "http" } else { "background" }, trace_id = self.id(), instance = self.0.instance.as_str(), route = self.0.route.as_str(), method = self.0.method.as_str(), actor_kind = c.actor.as_str(), repository = c.repository.as_ref().map(Id::as_str), request_id = c.request.as_ref().map(Id::as_str), transaction_id = c.transaction.as_ref().map(Id::as_str), event_id = c.event.as_ref().map(Id::as_str), phase, outcome, http_status = status, body_bytes_yielded = if phase == "none" { bytes } else { 0 }, work_bytes = if phase == "none" { 0 } else { bytes }, elapsed_seconds = elapsed);
        });
    }
    pub fn phase(&self, phase: Phase) -> PhaseGuard {
        PhaseGuard {
            trace: Some(self.clone()),
            phase,
            start: Instant::now(),
            outcome: Outcome::Interrupted,
            bytes: 0,
        }
    }
}

pub struct PhaseGuard {
    trace: Option<Trace>,
    phase: Phase,
    start: Instant,
    outcome: Outcome,
    bytes: u64,
}
impl PhaseGuard {
    pub fn current(phase: Phase) -> Self {
        Self {
            trace: Trace::current(),
            phase,
            start: Instant::now(),
            outcome: Outcome::Interrupted,
            bytes: 0,
        }
    }
    pub fn add_bytes(&mut self, bytes: u64) {
        self.bytes = self.bytes.saturating_add(bytes);
    }
    pub fn finish(mut self, outcome: Outcome) {
        self.outcome = outcome;
    }
}
impl Drop for PhaseGuard {
    fn drop(&mut self) {
        if let Some(trace) = &self.trace {
            let elapsed = self.start.elapsed().as_secs_f64();
            let labels = [self.phase.as_str(), self.outcome.as_str()];
            trace
                .0
                .metrics
                .phases
                .with_label_values(&labels)
                .observe(elapsed);
            trace.0.metrics.outcomes.with_label_values(&labels).inc();
            trace
                .0
                .metrics
                .work_bytes
                .with_label_values(&[self.phase.as_str()])
                .inc_by(self.bytes);
            trace.event(
                "work.ended",
                self.phase.as_str(),
                self.outcome.as_str(),
                0,
                self.bytes,
                elapsed,
            );
        }
    }
}
