mod web;
use crate::{NativeOptions, Output, RequestAction, Route, execute_native, parse_route};
use alt_remote_protocol::{ErrorCode, Failure, Limits, RequestKey, Retry};
use alt_server_store::{ByteStore, Error, InstanceSpec, Store};
use base64::Engine;
use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response,
    body::{Frame, Incoming},
    header,
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    convert::Infallible,
    io::{Seek, SeekFrom},
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::Semaphore};
pub use web::WebOptions;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type ResponseBody = UnsyncBoxBody<Bytes, BoxError>;

/// Persisted instance identity is required in every mode. Opening never calls
/// initialization or local bootstrap when configuration/authorization is absent.
#[derive(Clone)]
pub struct StoreFactory {
    pub database: postgres::Config,
    pub bytes: ByteStore,
    pub instance: InstanceSpec,
    pub signing_key: ed25519_dalek::SigningKey,
    pub limits: Limits,
    pub connect_timeout: Duration,
    pub statement_timeout: Duration,
}
impl StoreFactory {
    pub fn open(&self) -> alt_server_store::Result<Store> {
        let mut config = self.database.clone();
        config.connect_timeout(self.connect_timeout);
        let mut client = config.connect(postgres::NoTls)?;
        let timeout = self.statement_timeout.as_millis();
        if timeout == 0 || timeout > 300_000 {
            return Err(Error::Limit("database statement timeout"));
        }
        client.batch_execute(&format!(
            "SET statement_timeout = {timeout}; SET lock_timeout = {timeout}"
        ))?;
        Store::open(
            client,
            self.bytes.clone(),
            self.instance.clone(),
            self.signing_key.clone(),
            self.limits,
        )
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
struct State {
    factory: StoreFactory,
    options: HttpOptions,
    workers: Arc<Semaphore>,
    browser: Option<WebOptions>,
    passwords: Arc<Semaphore>,
}
fn full(bytes: impl Into<Bytes>) -> ResponseBody {
    Full::new(bytes.into())
        .map_err(|never: Infallible| match never {})
        .boxed_unsync()
}
fn response(status: u16, content_type: &str, body: ResponseBody) -> Response<ResponseBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(body)
        .expect("static response headers")
}
fn failure(
    status: u16,
    code: ErrorCode,
    message: &str,
    request: Option<RequestKey>,
    retry: Retry,
) -> Response<ResponseBody> {
    let body = serde_json::to_vec(&Failure {
        code,
        message: message.into(),
        request,
        retry,
    })
    .expect("failure JSON");
    let mut output = response(status, "application/json", full(body));
    if status == 401 {
        output.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            "Basic realm=\"alt\", charset=\"UTF-8\"".parse().unwrap(),
        );
    }
    output
}
fn store_failure(error: Error, key: Option<RequestKey>) -> Response<ResponseBody> {
    // PostgreSQL/I/O details may contain internal paths, SQL, connection
    // settings or credentials. Client messages are stable and redacted.
    match error {
        Error::Unauthorized => failure(
            403,
            ErrorCode::Forbidden,
            "Access denied",
            key,
            Retry::Never,
        ),
        Error::OutcomeUnknown => failure(
            503,
            ErrorCode::OutcomeUnknown,
            "Query the original request to determine its outcome",
            key,
            Retry::QueryOutcome,
        ),
        Error::Limit(_)
        | Error::Repository(alt_repository_service::Error::Limit(_))
        | Error::Task(alt_task_service::Error::Limit(_)) => failure(
            413,
            ErrorCode::QuotaExceeded,
            "Request exceeds an instance limit",
            key,
            Retry::NewIntent,
        ),
        Error::Precondition(_)
        | Error::Repository(alt_repository_service::Error::Precondition(_)) => failure(
            409,
            ErrorCode::PreconditionFailed,
            "The request precondition is not satisfied; inspect its current state",
            key,
            Retry::SameRequest,
        ),
        Error::Repository(alt_repository_service::Error::Forbidden(_)) => failure(
            403,
            ErrorCode::Forbidden,
            "Publication is not permitted by current policy",
            key,
            Retry::NewIntent,
        ),
        Error::Protocol(_) | Error::Json(_) => failure(
            400,
            ErrorCode::InvalidRequest,
            "Invalid request encoding or protocol fields",
            key,
            Retry::Never,
        ),
        Error::Integrity(_) | Error::Task(_) | Error::Repository(_) => failure(
            422,
            ErrorCode::IntegrityError,
            "Selected content or dependency validation failed",
            key,
            Retry::NewIntent,
        ),
        Error::Database(_) | Error::Io(_) => failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "Instance storage is temporarily unavailable",
            key,
            Retry::SameRequest,
        ),
    }
}
pub fn authorization(headers: &hyper::HeaderMap) -> Option<String> {
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
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
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
fn key(route: &Route) -> Option<RequestKey> {
    match route {
        Route::Request(key, _) => Some(key.clone()),
        _ => None,
    }
}
async fn handle(
    request: Request<Incoming>,
    state: Arc<State>,
    peer: std::net::IpAddr,
) -> Response<ResponseBody> {
    if request.method() == hyper::Method::GET
        && request.uri().path() == "/livez"
        && request.uri().query().is_none()
    {
        return response(200, "application/json", full("{\"alive\":true}"));
    }
    if state.browser.as_ref().is_some_and(|browser| {
        request.uri().path().starts_with("/web/")
            || browser.assets.get(request.uri().path()).is_some()
    }) {
        return web::handle(request, state, peer).await;
    }
    let Some(route) = parse_route(
        request.method().as_str(),
        request.uri().path(),
        request.uri().query(),
        &state.factory.instance.installation_id,
    ) else {
        return failure(
            404,
            ErrorCode::InvalidRequest,
            "Unknown endpoint",
            None,
            Retry::Never,
        );
    };
    let request_key = key(&route);
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
        let factory = state.factory.clone();
        return match tokio::time::timeout(
            state.options.work_timeout,
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                factory.open().map(|_| ())
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
        };
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
    if request.headers().contains_key(header::CONTENT_ENCODING) {
        return failure(
            415,
            ErrorCode::InvalidRequest,
            "Content encoding is not supported",
            request_key,
            Retry::Never,
        );
    }
    if matches!(
        route,
        Route::Begin(_) | Route::Request(_, RequestAction::Approve)
    ) && (request
        .headers()
        .get_all(header::CONTENT_TYPE)
        .iter()
        .count()
        != 1
        || request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_none_or(|value| !value.trim().eq_ignore_ascii_case("application/json")))
    {
        return failure(
            415,
            ErrorCode::InvalidRequest,
            "This endpoint requires application/json",
            request_key,
            Retry::Never,
        );
    }
    let factory = state.factory.clone();
    let selected_route = route.clone();
    let prepared = tokio::time::timeout(
        state.options.work_timeout,
        tokio::task::spawn_blocking(move || {
            let mut store = factory.open()?;
            let credential = store.authenticate(&token)?;
            let size = match &selected_route {
                Route::GitReceive(repository) => {
                    store.check_git_receive_access(&credential, repository)?;
                    store
                        .limits()
                        .total_bytes
                        .checked_add(store.limits().request_bytes as u64)
                        .ok_or(Error::Limit("Git body bytes"))?
                }
                Route::GitUpload(repository) => {
                    store.repository_info(&credential, repository)?;
                    store.limits().request_bytes as u64
                }
                Route::GitAdvertise(repository, _) => {
                    store.repository_info(&credential, repository)?;
                    0
                }
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
            Ok::<_, Error>((credential, permit, size))
        }),
    )
    .await;
    let (credential, permit, size) = match prepared {
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
        Ok(Err(BodyError::Limit)) => {
            return failure(
                413,
                ErrorCode::QuotaExceeded,
                "Request body exceeds the selected bound",
                request_key,
                Retry::Never,
            );
        }
        Ok(Err(BodyError::Storage)) => {
            return failure(
                503,
                ErrorCode::TemporarilyUnavailable,
                "Request storage is temporarily unavailable",
                request_key,
                Retry::SameRequest,
            );
        }
        Ok(Err(BodyError::Malformed)) => {
            return failure(
                400,
                ErrorCode::InvalidRequest,
                "Request body is incomplete or contains unsupported trailers",
                request_key,
                Retry::SameRequest,
            );
        }
        Err(_) => {
            return failure(
                408,
                ErrorCode::TemporarilyUnavailable,
                "Request body deadline exceeded",
                request_key,
                Retry::SameRequest,
            );
        }
    };
    let options = state.options.native.clone();
    let factory = state.factory.clone();
    let result = tokio::time::timeout(
        state.options.work_timeout,
        tokio::task::spawn_blocking(move || {
            // This permit remains held if the HTTP waiter times out. Dropping a
            // response cannot start unlimited detached database/validation work.
            let _permit = permit;
            let mut store = factory.open()?;
            source.as_file_mut().seek(SeekFrom::Start(0))?;
            match route {
                Route::GitAdvertise(..) | Route::GitUpload(..) | Route::GitReceive(..) => {
                    crate::execute_git(
                        &mut store,
                        &credential,
                        route,
                        protocol_v2,
                        source.as_file_mut(),
                    )
                }
                _ => execute_native(
                    &mut store,
                    &credential,
                    &options,
                    route,
                    source.as_file_mut(),
                ),
            }
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
fn render_output(
    result: alt_server_store::Result<Output>,
    request_key: Option<RequestKey>,
) -> Response<ResponseBody> {
    match result {
        Ok(Output::Bytes {
            content_type,
            bytes,
        }) => response(200, content_type, full(bytes)),
        Ok(Output::GitReport { bytes, request }) => {
            let mut output = response(200, "application/x-git-receive-pack-result", full(bytes));
            let location = format!(
                "/v1/repositories/{}/epochs/{}/requests/{}/{}",
                request.scope.repository_instance_id,
                request.scope.recovery_epoch,
                request.actor_id,
                request.request_id
            );
            output.headers_mut().insert(
                "Alt-Request-Location",
                location.parse().expect("validated request path"),
            );
            output
        }
        Ok(Output::Temporary {
            content_type,
            bytes,
            file,
        }) => {
            let stream = tokio_util::io::ReaderStream::with_capacity(
                tokio::fs::File::from_std(file.into_file()),
                65536,
            )
            .map_ok(Frame::data)
            .map_err(|e| -> BoxError { Box::new(e) });
            let mut output = response(200, content_type, StreamBody::new(stream).boxed_unsync());
            output
                .headers_mut()
                .insert(header::CONTENT_LENGTH, bytes.into());
            output
        }
        Ok(Output::Json(value)) => match serde_json::to_vec(&value) {
            Ok(bytes) => response(200, "application/json", full(bytes)),
            Err(_) => failure(
                500,
                ErrorCode::TemporarilyUnavailable,
                "Response encoding failed",
                request_key,
                Retry::QueryOutcome,
            ),
        },
        Ok(Output::File {
            bytes,
            digest,
            file,
        }) => {
            let stream =
                tokio_util::io::ReaderStream::with_capacity(tokio::fs::File::from_std(file), 65536)
                    .map_ok(Frame::data)
                    .map_err(|e| -> BoxError { Box::new(e) });
            let mut output = response(
                200,
                "application/octet-stream",
                StreamBody::new(stream).boxed_unsync(),
            );
            output
                .headers_mut()
                .insert(header::CONTENT_LENGTH, bytes.into());
            output.headers_mut().insert(
                "Alt-Content-Blake3",
                digest.parse().expect("validated digest"),
            );
            output
        }
        Err(error) => store_failure(error, request_key),
    }
}

enum BodyError {
    Limit,
    Malformed,
    Storage,
}
impl From<std::io::Error> for BodyError {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
async fn spool(mut body: Incoming, max: u64) -> Result<tempfile::NamedTempFile, BodyError> {
    let temp = tempfile::NamedTempFile::new()?;
    let mut file = tokio::fs::File::from_std(temp.as_file().try_clone()?);
    let mut remaining = max;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| BodyError::Malformed)?;
        let data = frame.into_data().map_err(|_| BodyError::Malformed)?;
        remaining = remaining
            .checked_sub(data.len() as u64)
            .ok_or(BodyError::Limit)?;
        file.write_all(&data).await?;
    }
    file.flush().await?;
    Ok(temp)
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
    if let Some(ref browser) = browser {
        browser.validate()?;
    }
    let check_factory = factory.clone();
    let origin = browser.as_ref().map(|b| b.origin.clone());
    tokio::task::spawn_blocking(move || {
        check_factory.open()?.check_browser_binding(origin.as_ref())
    })
    .await
    .map_err(|_| std::io::Error::other("browser binding check unavailable"))?
    .map_err(|_| std::io::Error::other("browser binding does not match configuration"))?;
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
    let connections = Arc::new(Semaphore::new(options.connections));
    let state = Arc::new(State {
        factory,
        workers: Arc::new(Semaphore::new(options.workers)),
        options,
        browser,
        passwords: Arc::new(Semaphore::new(2)),
    });
    tokio::pin!(shutdown);
    loop {
        let (socket, peer) = tokio::select! { _ = &mut shutdown => return Ok(()), accepted = listener.accept() => accepted? };
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            drop(socket);
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut builder = http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(state.options.header_timeout)
                .max_headers(state.options.headers)
                .max_buf_size(state.options.header_bytes);
            let lifetime = state.options.connection_timeout;
            let service = service_fn(move |request| {
                let state = state.clone();
                async move { Ok::<_, Infallible>(handle(request, state, peer.ip()).await) }
            });
            let _ = tokio::time::timeout(
                lifetime,
                builder.serve_connection(TokioIo::new(socket), service),
            )
            .await;
        });
    }
}
