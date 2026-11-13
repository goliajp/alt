//! One request from its route to its response: authority, body, execution.
use super::*;
use alt_server_store::{Credential, PackView};

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
            let (credential, size, view) =
                prepare(&factory, &selected_route, &selected_options, &request_token)?;
            Ok::<_, Error>((credential, permit, size, view))
        }),
    )
    .await;
    let (credential, permit, size, view) = match prepared {
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
    if let Some(length) = request.headers().get(header::CONTENT_LENGTH) {
        let declared = length.to_str().ok().and_then(|s| s.parse::<u64>().ok());
        if declared.is_none_or(|n| n > size) {
            return failure(
                413,
                ErrorCode::QuotaExceeded,
                "Request body exceeds the selected bound",
                request_key,
                Retry::Never,
            );
        }
    }
    let source =
        tokio::time::timeout(state.options.body_timeout, spool(request.into_body(), size)).await;
    let mut source = match source {
        Ok(Ok(source)) => source,
        Ok(Err(error)) => return body_failure(Some(error), request_key),
        Err(_) => return body_failure(None, request_key),
    };
    let factory = state.factory.clone();
    let work = Work {
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
/// Authenticates the caller for the route and bounds the body it may send. A Git
/// request gets its repository view from the same statement.
fn prepare(
    factory: &pool::Pool,
    route: &Route,
    options: &NativeOptions,
    token: &str,
) -> alt_server_store::Result<(Credential, u64, Option<PackView>)> {
    let mut store = factory.open()?;
    let (credential, view) = match route {
        Route::GitReceive(repository)
        | Route::GitUpload(repository)
        | Route::GitAdvertise(repository, _) => {
            let (credential, view) =
                factory.git_open(&mut store, token, repository, crate::git::ref_filter(route))?;
            (credential, Some(view))
        }
        _ => (factory.authenticate(&mut store, token)?, None),
    };
    let size = match route {
        Route::ChangeLink(route) => {
            crate::change_links::api::request_bytes(&mut store, &credential, route)?
        }
        Route::Portable(route) => {
            crate::portable::request_bytes(&mut store, &credential, options, route)?
        }
        Route::IssueAgent => 16_384,
        Route::GitReceive(_) => {
            if !view.as_ref().is_some_and(|view| view.writable) {
                return Err(Error::Unauthorized);
            }
            store
                .limits()
                .total_bytes
                .checked_add(store.limits().request_bytes as u64)
                .ok_or(Error::Limit("Git body bytes"))?
        }
        Route::GitUpload(_) => store.limits().request_bytes as u64,
        Route::GitAdvertise(..) => 0,
        Route::Request(key, RequestAction::Upload(digest)) => {
            let entries = store.selected_upload(&credential, key, digest)?;
            entries[0].bytes
        }
        Route::Request(key, RequestAction::UploadChunk(digest)) => {
            store.selected_chunk(&credential, key, digest)?
        }
        Route::Begin(scope) => {
            let info = store.repository_info(&credential, &scope.repository_instance_id)?;
            if info.scope != *scope {
                return Err(Error::Unauthorized);
            }
            store.limits().request_bytes as u64
        }
        Route::Request(_, RequestAction::Approve) => store.limits().request_bytes as u64,
        _ => 0,
    };
    Ok((credential, size, view))
}
/// what the execution of an authorized request needs besides its body
struct Work {
    factory: pool::Pool,
    credential: Credential,
    token: String,
    options: NativeOptions,
    metrics: Arc<alt_server_telemetry::Metrics>,
    compaction: crate::compaction::Compaction,
    protocol_v2: bool,
    view: Option<PackView>,
}
impl Work {
    fn run(
        self,
        store: &mut Store,
        route: Route,
        source: &mut std::fs::File,
    ) -> alt_server_store::Result<Output> {
        match route {
            Route::Metrics => observation::metrics(store, &self.credential, &self.metrics),
            Route::ChangeLink(route) => {
                let current = self.factory.authenticate(store, &self.token)?;
                crate::change_links::api::execute(
                    store,
                    &current,
                    &self.factory.change_links,
                    route,
                    source,
                )
            }
            Route::Portable(route) => {
                let current = self.factory.authenticate(store, &self.token)?;
                crate::portable::execute(
                    store,
                    &current,
                    &self.options,
                    route,
                    source,
                    &mut |store| self.factory.authenticate(store, &self.token),
                )
            }
            Route::GitAdvertise(..) | Route::GitUpload(..) | Route::GitReceive(..) => {
                let pushed = match &route {
                    Route::GitReceive(repository) => Some(repository.clone()),
                    _ => None,
                };
                let output = crate::git::execute_git_with_cache(
                    store,
                    &self.credential,
                    route,
                    self.protocol_v2,
                    source,
                    self.view.ok_or(Error::Integrity("Git repository view"))?,
                )?;
                if let Some(repository) = pushed {
                    self.compaction.published(&repository);
                }
                Ok(output)
            }
            _ => {
                // a native publication stores its Git objects as a segment too
                let published = match &route {
                    Route::Request(key, _) => Some(key.scope.repository_instance_id.clone()),
                    _ => None,
                };
                let output = execute_native(store, &self.credential, &self.options, route, source)?;
                if let Some(repository) = published {
                    self.compaction.published(&repository);
                }
                Ok(output)
            }
        }
    }
}
/// the response to a body that could not be received; no error means it ran out of time
fn body_failure(error: Option<BodyError>, key: Option<RequestKey>) -> Response<ResponseBody> {
    let (status, code, message, retry) = match error {
        Some(BodyError::Limit) => (
            413,
            ErrorCode::QuotaExceeded,
            "Request body exceeds the selected bound",
            Retry::Never,
        ),
        Some(BodyError::Storage) => (
            503,
            ErrorCode::TemporarilyUnavailable,
            "Request storage is temporarily unavailable",
            Retry::SameRequest,
        ),
        Some(BodyError::Malformed) => (
            400,
            ErrorCode::InvalidRequest,
            "Request body is incomplete or contains unsupported trailers",
            Retry::SameRequest,
        ),
        None => (
            408,
            ErrorCode::TemporarilyUnavailable,
            "Request body deadline exceeded",
            Retry::SameRequest,
        ),
    };
    failure(status, code, message, key, retry)
}
