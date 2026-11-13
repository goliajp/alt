//! The routing table. A pattern only selects the handler: method, query string and
//! the strict path rules are checked against the raw request target, so an
//! encoded, overlong or otherwise noncanonical path is an unknown endpoint.
use super::*;
use alt_server_telemetry::Route as MetricRoute;
use axum::{
    Router,
    extract::{ConnectInfo, State},
    routing::any,
};
use observation::{respond, start};
use std::net::SocketAddr;

type Reply = Response<Body>;

pub(super) fn router(state: Arc<App>) -> Router {
    let mut router = Router::new()
        .route("/livez", any(livez))
        .route("/healthz", any(native))
        .route("/oauth/device/code", any(device))
        .route("/oauth/token", any(device))
        .route("/oauth/revoke", any(device))
        .route("/cloud/runtime/v1/health", any(provision))
        .route("/git/{repository}/info/refs", any(native))
        .route("/git/{repository}/git-upload-pack", any(native))
        .route("/git/{repository}/git-receive-pack", any(native))
        .route("/v1/{*rest}", any(native))
        .route("/api/v1/change-links/inbox", any(native))
        .fallback(resource);
    if state.browser.is_some() {
        router = router
            .route("/web/", any(browser))
            .route("/web/{*rest}", any(browser));
    }
    router.with_state(state)
}

async fn livez(State(state): State<Arc<App>>, request: Request) -> Reply {
    let trace = start(&state, request.method(), MetricRoute::Health);
    let alive = request.method() == Method::GET && request.uri().query().is_none();
    respond(trace, async move {
        if alive {
            response(200, "application/json", full("{\"alive\":true}"))
        } else {
            unknown()
        }
    })
    .await
}
async fn device(
    State(state): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Reply {
    let trace = start(&state, request.method(), MetricRoute::Device);
    respond(trace, device::handle(request, state, peer.ip())).await
}
async fn provision(State(state): State<Arc<App>>, request: Request) -> Reply {
    let trace = start(&state, request.method(), MetricRoute::Provision);
    respond(trace, provision::handle(request, state)).await
}
async fn browser(
    State(state): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Reply {
    let trace = start(&state, request.method(), MetricRoute::Browser);
    respond(trace, web::handle(request, state, peer.ip())).await
}
/// a file of the frozen browser bundle is served under its path whatever else that path matches
fn is_asset(state: &App, request: &Request) -> bool {
    state
        .browser
        .as_ref()
        .is_some_and(|browser| browser.assets.get(request.uri().path()).is_some())
}
async fn asset(state: Arc<App>, request: Request) -> Reply {
    let trace = start(&state, request.method(), MetricRoute::Asset);
    respond(trace, async move { web::asset(&state, &request) }).await
}
async fn native(State(state): State<Arc<App>>, request: Request) -> Reply {
    if is_asset(&state, &request) {
        return asset(state, request).await;
    }
    let route = parse_route(
        request.method().as_str(),
        request.uri().path(),
        request.uri().query(),
        &state.factory.instance.installation_id,
    );
    let category = route
        .as_ref()
        .map_or(MetricRoute::Unknown, observation::category);
    let trace = start(&state, request.method(), category);
    if let Some(route) = &route {
        observation::native(&trace, route);
    }
    respond(trace, handle::handle(request, state, route)).await
}
async fn resource(State(state): State<Arc<App>>, request: Request) -> Reply {
    if is_asset(&state, &request) {
        return asset(state, request).await;
    }
    let trace = start(&state, request.method(), MetricRoute::Unknown);
    respond(trace, async { unknown() }).await
}
