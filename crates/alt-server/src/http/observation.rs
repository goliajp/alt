//! The async request owns its trace explicitly. Only blocking work uses the
//! synchronous scope, so unrelated tasks on the same executor cannot inherit it.
use super::*;
use crate::web_route::WebRoute;
use alt_server_telemetry::Route as MetricRoute;
use alt_server_telemetry::{Id, Method, Metrics, ResponseObservation, Trace};

tokio::task_local! { static REQUEST_TRACE: Trace; }
pub(crate) fn current() -> Option<Trace> {
    REQUEST_TRACE
        .try_with(Clone::clone)
        .ok()
        .or_else(Trace::current)
}
pub(crate) async fn scope<T>(trace: Trace, work: impl std::future::Future<Output = T>) -> T {
    REQUEST_TRACE.scope(trace, work).await
}
pub(crate) fn spawn_blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    let trace = current();
    tokio::task::spawn_blocking(move || match trace {
        Some(trace) => trace.in_scope(work),
        None => work(),
    })
}
fn bind_scope(trace: &Trace, scope: &alt_remote_protocol::Scope) {
    trace.identity(Id::new(&scope.repository_instance_id), None, None);
}
fn bind_key(trace: &Trace, key: &RequestKey) {
    trace.identity(
        Id::new(&key.scope.repository_instance_id),
        Id::new(&key.request_id),
        None,
    );
}
fn portable(trace: &Trace, route: &crate::PortableRoute) {
    match route {
        crate::PortableRoute::Capabilities => (),
        crate::PortableRoute::Inspect(scope) => bind_scope(trace, scope),
        crate::PortableRoute::Export(scope, id, _) => {
            bind_scope(trace, scope);
            trace.identity(None, Id::new(id), None);
        }
        crate::PortableRoute::Import(key, _) => bind_key(trace, key),
    }
}
fn change(trace: &Trace, route: &crate::change_links::route::ChangeRoute) {
    if let crate::change_links::route::ChangeRoute::Scoped(scope, _) = route {
        bind_scope(trace, scope);
    }
    if let Some(key) = route.request_key() {
        bind_key(trace, &key);
    }
}
fn native(trace: &Trace, route: &Route) {
    match route {
        Route::Request(key, _) => bind_key(trace, key),
        Route::Begin(scope)
        | Route::Content(scope, _)
        | Route::Bundles(scope, ..)
        | Route::Records(scope, ..) => bind_scope(trace, scope),
        Route::Repository(repository)
        | Route::GitAdvertise(repository, _)
        | Route::GitUpload(repository)
        | Route::GitReceive(repository) => trace.identity(Id::new(repository), None, None),
        Route::Portable(route) => portable(trace, route),
        Route::ChangeLink(route) => change(trace, route),
        _ => (),
    }
}
pub(super) fn web(route: &WebRoute) {
    let Some(trace) = current() else {
        return;
    };
    match route {
        WebRoute::Repository(repository)
        | WebRoute::Records(repository, ..)
        | WebRoute::Record(repository, ..)
        | WebRoute::History(repository, ..)
        | WebRoute::Tree(repository, ..)
        | WebRoute::Object(repository, ..)
        | WebRoute::Content(repository, ..) => trace.identity(Id::new(repository), None, None),
        WebRoute::Review(key) | WebRoute::Approve(key) | WebRoute::ReviewContent(key, _) => {
            bind_key(&trace, key)
        }
        WebRoute::Portable(route) => portable(&trace, route),
        WebRoute::ChangeLink(route) => change(&trace, route),
        _ => (),
    }
}
fn category(route: &Route) -> MetricRoute {
    match route {
        Route::Health => MetricRoute::Health,
        Route::Metrics => MetricRoute::Metrics,
        Route::Capabilities | Route::Principal => MetricRoute::Capabilities,
        Route::Delegations(_)
        | Route::IssueAgent
        | Route::Delegation(_)
        | Route::RevokeAgent(_) => MetricRoute::Delegation,
        Route::Repository(_) | Route::Content(..) | Route::Bundles(..) | Route::Records(..) => {
            MetricRoute::Repository
        }
        Route::Begin(_) => MetricRoute::Request,
        Route::Request(
            _,
            RequestAction::Upload(_)
            | RequestAction::UploadChunk(_)
            | RequestAction::UploadStatus
            | RequestAction::Assemble(_),
        ) => MetricRoute::Upload,
        Route::Request(
            _,
            RequestAction::Prepare | RequestAction::Publish | RequestAction::Approve,
        ) => MetricRoute::Publication,
        Route::Request(..) => MetricRoute::Request,
        Route::GitAdvertise(..) => MetricRoute::GitAdvertise,
        Route::GitUpload(_) => MetricRoute::GitUpload,
        Route::GitReceive(_) => MetricRoute::GitReceive,
        Route::Portable(_) => MetricRoute::Portable,
        Route::ChangeLink(_) => MetricRoute::ChangeLink,
    }
}
pub(super) async fn handle(
    request: Request<Incoming>,
    state: Arc<State>,
    peer: std::net::IpAddr,
) -> Response<ResponseBody> {
    let path = request.uri().path();
    let native_route = parse_route(
        request.method().as_str(),
        path,
        request.uri().query(),
        &state.factory.instance.installation_id,
    );
    let route = if path == "/cloud/runtime/v1/health" {
        MetricRoute::Provision
    } else if matches!(
        path,
        "/oauth/device/code" | "/oauth/token" | "/oauth/revoke"
    ) {
        MetricRoute::Device
    } else if path == "/livez" {
        MetricRoute::Health
    } else if state.browser.is_some() && path.starts_with("/web/") {
        MetricRoute::Browser
    } else if state
        .browser
        .as_ref()
        .is_some_and(|b| b.assets.get(path).is_some())
    {
        MetricRoute::Asset
    } else {
        native_route
            .as_ref()
            .map(category)
            .unwrap_or(MetricRoute::Unknown)
    };
    let trace = Trace::start(
        state.metrics.clone(),
        Id::new(&state.factory.instance.installation_id).expect("validated instance identity"),
        route,
        Method::from_http(request.method().as_str()),
    );
    if let Some(route) = native_route {
        native(&trace, &route);
    }
    let observation = ResponseObservation::new(trace.clone());
    let trace_header =
        hyper::header::HeaderValue::from_str(trace.id()).expect("generated trace identifier");
    let response = REQUEST_TRACE
        .scope(trace, super::handle(request, state, peer))
        .await;
    let (mut parts, body) = response.into_parts();
    parts.headers.insert("x-alt-trace-id", trace_header);
    let status = parts.status.as_u16();
    Response::from_parts(parts, observation.body(body, status).boxed_unsync())
}

pub(super) fn metrics(
    store: &mut Store,
    credential: &alt_server_store::Credential,
    metrics: &Metrics,
) -> alt_server_store::Result<Output> {
    // This is the same current administrator check as the operator/browser
    // status API. Neither a scrape nor the process registry bypasses Cloud auth.
    let status = store.instance_status(credential)?;
    let capacity = store.bytes().capacity()?;
    let number = |value: &str| {
        value
            .parse::<u64>()
            .map_err(|_| Error::Integrity("instance observation counter"))
    };
    let snapshot = alt_server_telemetry::Snapshot {
        observed_at_unix_ms: number(&status.observed_at_unix_ms)?,
        stored_bytes: number(&status.stored_bytes)?,
        reserved_bytes: number(&status.reserved_bytes)?,
        pending_requests: number(&status.pending_requests)?,
        recovery_gaps: number(&status.recovery_gaps)?,
        pending_events: number(&status.pending_events)?,
        pending_change_links: number(&status.pending_change_links)?,
        recovery_change_links: number(&status.recovery_change_links)?,
        volume_total_bytes: capacity.total_bytes,
        volume_available_bytes: capacity.available_bytes,
        last_backup_created_unix_ms: status
            .last_backup
            .as_ref()
            .map(|entry| number(&entry.occurred_at_unix_ms))
            .transpose()?,
    };
    Ok(Output::Bytes {
        content_type: "text/plain; version=0.0.4; charset=utf-8",
        bytes: metrics
            .encode_snapshot(&snapshot)
            .map_err(|_| Error::Integrity("metrics encoding"))?,
    })
}
