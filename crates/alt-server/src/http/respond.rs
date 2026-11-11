//! Responses: protocol failures, store errors and the outputs of executed requests.
use super::*;

pub(super) fn full(bytes: impl Into<Bytes>) -> ResponseBody {
    Full::new(bytes.into())
        .map_err(|never: Infallible| match never {})
        .boxed_unsync()
}
pub(super) fn response(
    status: u16,
    content_type: &str,
    body: ResponseBody,
) -> Response<ResponseBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(body)
        .expect("static response headers")
}
pub(super) fn failure(
    status: u16,
    code: ErrorCode,
    message: &str,
    request: Option<RequestKey>,
    retry: Retry,
) -> Response<ResponseBody> {
    let body = alt_json::to_vec(&Failure {
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
pub(super) fn store_failure(error: Error, key: Option<RequestKey>) -> Response<ResponseBody> {
    // PostgreSQL/I/O details may contain internal paths, SQL, connection
    // settings or credentials. Client messages are stable and redacted.
    match error {
        Error::AuthorityUnavailable => failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "Current account authority could not be verified; retry when the platform is available",
            key,
            Retry::SameRequest,
        ),
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
        | Error::Portable(alt_portable::Error::Limit(_))
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
        Error::Portable(alt_portable::Error::Io(_)) => failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "Instance storage is temporarily unavailable",
            key,
            Retry::SameRequest,
        ),
        Error::Integrity(_) | Error::Task(_) | Error::Repository(_) | Error::Portable(_) => {
            failure(
                422,
                ErrorCode::IntegrityError,
                "Selected content or dependency validation failed",
                key,
                Retry::NewIntent,
            )
        }
        Error::Database(_) | Error::Io(_) => failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "Instance storage is temporarily unavailable",
            key,
            Retry::SameRequest,
        ),
    }
}
pub(super) fn render_output(
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
        Ok(Output::WholePack(pieces, checksum)) => response(
            200,
            "application/x-git-upload-pack-result",
            pack_body::PackBody::new(pieces, checksum)
                .map_err(|e| -> BoxError { Box::new(e) })
                .boxed_unsync(),
        ),
        Ok(Output::Temporary {
            content_type,
            bytes,
            file,
        }) => {
            let body = file_body::FileBody::new(tokio::fs::File::from_std(file.into_file()), bytes)
                .map_err(|e| -> BoxError { Box::new(e) })
                .boxed_unsync();
            let mut output = response(200, content_type, body);
            output
                .headers_mut()
                .insert(header::CONTENT_LENGTH, bytes.into());
            output
        }
        Ok(Output::Json(value)) => match alt_json::to_vec(&value) {
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
            let file = match file.into_file() {
                Ok(file) => file,
                Err(error) => return store_failure(error.into(), request_key),
            };
            let body = file_body::FileBody::new(tokio::fs::File::from_std(file), bytes)
                .map_err(|e| -> BoxError { Box::new(e) })
                .boxed_unsync();
            let mut output = response(200, "application/octet-stream", body);
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
