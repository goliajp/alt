//! One request from its route to its response: authority, body, execution.
use super::*;

pub(super) fn key(route: &Route) -> Option<RequestKey> {
    match route {
        Route::ChangeLink(route) => route.request_key(),
        Route::Request(key, _) | Route::Portable(crate::PortableRoute::Import(key, _)) => {
            Some(key.clone())
        }
        _ => None,
    }
}
pub(super) async fn handle(
    request: Request,
    state: Arc<App>,
    route: Option<Route>,
) -> Response<ResponseBody> {
    let Some(route) = route else {
        return unknown();
    };
    let request_key = key(&route);
    if matches!(
        route,
        Route::IssueAgent
            | Route::Delegations(_)
            | Route::Delegation(_)
            | Route::RevokeAgent(_)
            | Route::ChangeLink(_)
    ) && (request.headers().contains_key(header::COOKIE)
        || request.headers().contains_key(header::ORIGIN))
    {
        return failure(
            403,
            ErrorCode::Forbidden,
            "Use a terminal credential for native delegation requests",
            None,
            Retry::Never,
        );
    }
    let protocol_v2 = request.headers().get_all("Git-Protocol").iter().count() == 1
        && request
            .headers()
            .get("Git-Protocol")
            .and_then(|value| value.to_str().ok())
            == Some("version=2");
    let permit = match state.workers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return failure(
                503,
                ErrorCode::TemporarilyUnavailable,
                "Instance is busy; retry the same request",
                request_key,
                Retry::SameRequest,
            );
        }
    };
    if route == Route::Health {
        return ready(&state, permit).await;
    }
    let Some(token) = authorization(request.headers()) else {
        return failure(
            401,
            ErrorCode::Unauthenticated,
            "Authentication required",
            request_key,
            Retry::Never,
        );
    };
    let gzip = request.headers().contains_key(header::CONTENT_ENCODING);
    let reading = matches!(*request.method(), Method::GET | Method::HEAD);
    if let Some(message) = unsupported_media(&route, request.headers(), gzip) {
        return failure(
            415,
            ErrorCode::InvalidRequest,
            message,
            request_key,
            Retry::Never,
        );
    }
    let factory = state.factory.clone();
    let selected_route = route.clone();
    let selected_options = state.options.native.clone();
    let request_token = token.clone();
    let prepared = tokio::time::timeout(
        state.options.work_timeout,
        spawn_observed(Phase::Authority, move || {
            // the worker is given back before the body is received
            let _permit = permit;
            work::prepare(&factory, &selected_route, &selected_options, &request_token)
        }),
    )
    .await;
    let work::Prepared {
        credential,
        caller,
        size,
        view,
    } = match prepared {
        Ok(Ok(Ok(value))) => value,
        Ok(Ok(Err(error))) => return store_failure(error, request_key),
        _ => {
            return failure(
                503,
                ErrorCode::TemporarilyUnavailable,
                "Instance is temporarily unavailable",
                request_key,
                Retry::SameRequest,
            );
        }
    };
    let length = request.headers().get(header::CONTENT_LENGTH);
    let declared = length.map(|length| length.to_str().ok().and_then(|s| s.parse::<u64>().ok()));
    if declared.is_some_and(|declared| declared.is_none_or(|n| n > size)) {
        return receiving::unreceived(BodyError::Limit, request_key);
    }
    let mut slot = match state
        .receiving
        .begin(&caller, declared.flatten().unwrap_or(0))
    {
        Ok(slot) => slot,
        Err(full) => return receiving::refusal(full, request_key),
    };
    let receipt = body::Receipt::of(&state, Some(&mut slot));
    let mut source = match spool(request.into_body(), size, receipt).await {
        Ok(source) => source,
        Err(error) => return receiving::unreceived(error, request_key),
    };
    // whole: from here the request waits for a worker like any that runs
    let permit = match receiving::worker(&state, request_key.clone()).await {
        Ok(permit) => permit,
        Err(refusal) => return *refusal,
    };
    drop(slot);
    let factory = state.factory.clone();
    let work = work::Work {
        factory: factory.clone(),
        credential,
        token,
        options: state.options.native.clone(),
        metrics: state.metrics.clone(),
        compaction: state.compaction.clone(),
        protocol_v2,
        view,
    };
    let result = tokio::time::timeout(
        state.options.work_timeout,
        spawn_observed(Phase::Execute, move || {
            // This permit remains held if the HTTP waiter times out. Dropping a
            // response cannot start unlimited detached database/validation work.
            let _permit = permit;
            if gzip {
                source = encoding::decode(source, size)?;
            }
            let mut store = factory.open()?;
            source.as_file_mut().seek(SeekFrom::Start(0))?;
            work.run(&mut store, route, source.as_file_mut())
        }),
    )
    .await;
    match result {
        Ok(Ok(result)) => render_output(result, request_key),
        // a read changed nothing, so there is no outcome to look up
        _ if reading => failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "The response did not finish in time; retry the same request",
            request_key,
            Retry::SameRequest,
        ),
        _ => failure(
            503,
            ErrorCode::OutcomeUnknown,
            "Processing may still be running; query the original request",
            request_key,
            Retry::QueryOutcome,
        ),
    }
}
/// readiness means the schema and instance rows are current right now
async fn ready(state: &App, permit: tokio::sync::OwnedSemaphorePermit) -> Response<ResponseBody> {
    let factory = state.factory.clone();
    match tokio::time::timeout(
        state.options.work_timeout,
        spawn_observed(Phase::Authority, move || {
            let _permit = permit;
            factory
                .open()
                .and_then(|mut store| store.check_connection())
        }),
    )
    .await
    {
        Ok(Ok(Ok(()))) => response(200, "application/json", full("{\"ready\":true}")),
        _ => failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "Instance is not ready",
            None,
            Retry::SameRequest,
        ),
    }
}
/// why the encoding or media type of the request body is refused for this route
fn unsupported_media(route: &Route, headers: &HeaderMap, gzip: bool) -> Option<&'static str> {
    if gzip
        && (!matches!(route, Route::GitUpload(_))
            || headers.get_all(header::CONTENT_ENCODING).iter().count() != 1
            || headers
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok())
                .is_none_or(|value| !value.trim().eq_ignore_ascii_case("gzip")))
    {
        return Some("Content encoding is not supported");
    }
    if (matches!(
        route,
        Route::Begin(_) | Route::IssueAgent | Route::Request(_, RequestAction::Approve)
    ) || matches!(route, Route::Portable(p) if p.has_json_body())
        || matches!(route, Route::ChangeLink(p) if p.has_json_body()))
        && (headers.get_all(header::CONTENT_TYPE).iter().count() != 1
            || headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .is_none_or(|value| !value.trim().eq_ignore_ascii_case("application/json")))
    {
        return Some("This endpoint requires application/json");
    }
    None
}
