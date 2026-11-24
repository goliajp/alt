//! Responses: protocol failures, store errors and the outputs of executed requests.
use super::*;
use hyper::body::{Frame, SizeHint};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

/// Every body a handler answers with, so that a response is boxed once, with its
/// observation, when it is handed to the router.
pub(super) enum ResponseBody {
    Full(Full<Bytes>),
    File(file_body::FileBody<tokio::fs::File>),
    Pack(pack_body::PackBody),
}
impl hyper::body::Body for ResponseBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match self.get_mut() {
            Self::Full(body) => Pin::new(body)
                .poll_frame(cx)
                .map_err(|never| match never {}),
            Self::File(body) => Pin::new(body).poll_frame(cx),
            Self::Pack(body) => Pin::new(body).poll_frame(cx),
        }
    }
    fn is_end_stream(&self) -> bool {
        match self {
            Self::Full(body) => body.is_end_stream(),
            Self::File(body) => body.is_end_stream(),
            Self::Pack(body) => body.is_end_stream(),
        }
    }
    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Full(body) => body.size_hint(),
            Self::File(body) => body.size_hint(),
            Self::Pack(body) => body.size_hint(),
        }
    }
}
pub(super) fn full(bytes: impl Into<Bytes>) -> ResponseBody {
    ResponseBody::Full(Full::new(bytes.into()))
}
/// A file of known length, declared in the headers, with what it has of the room
/// of responses kept until it is sent.
fn file(
    content_type: &str,
    (file, bytes): (std::fs::File, u64),
    held: Option<alt_shares::Share>,
) -> Response<ResponseBody> {
    let body = file_body::FileBody::new(tokio::fs::File::from_std(file), bytes).holding(held);
    let mut output = response(200, content_type, ResponseBody::File(body));
    output
        .headers_mut()
        .insert(header::CONTENT_LENGTH, bytes.into());
    output.extensions_mut().insert(connection::DeclaredLength);
    output
}
/// the answer to a request of an address that has as many under way as one may
pub(super) fn crowded() -> Response<ResponseBody> {
    let mut output = failure(
        429,
        ErrorCode::TemporarilyUnavailable,
        "Too many requests of this address are under way",
        None,
        Retry::SameRequest,
    );
    // a place is there again when any request of the address ends, and most
    // end within the second
    let soon = header::HeaderValue::from_static("1");
    output.headers_mut().insert(header::RETRY_AFTER, soon);
    output
}
pub(super) fn unknown() -> Response<ResponseBody> {
    failure(
        404,
        ErrorCode::InvalidRequest,
        "Unknown endpoint",
        None,
        Retry::Never,
    )
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
/// What a request the store refused is answered with, and after how many
/// seconds it may be sent again where that is known.
pub(super) fn store_failure(error: Error, key: Option<RequestKey>) -> Response<ResponseBody> {
    let again = match &error {
        Error::Attempts(left) => Some(left.div_ceil(1000)),
        // what it waited for is other pushes being verified: gone in less
        Error::Busy(_) => Some(60),
        _ => None,
    };
    let mut output = refused(error, key);
    if let Some(seconds) = again {
        let seconds = header::HeaderValue::from(seconds);
        output.headers_mut().insert(header::RETRY_AFTER, seconds);
    }
    output
}
/// PostgreSQL/I/O details may contain internal paths, SQL, connection
/// settings or credentials. Client messages are stable and redacted.
fn refused(error: Error, key: Option<RequestKey>) -> Response<ResponseBody> {
    match error {
        Error::AuthorityUnavailable => failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "Current account authority could not be verified; retry when the platform is available",
            key,
            Retry::SameRequest,
        ),
        Error::Busy(_) => failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "The instance is busy with other requests; retry the same request",
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
        | Error::Attempts(_)
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
            ResponseBody::Pack(pack_body::PackBody::new(pieces, checksum)),
        ),
        Ok(Output::Temporary {
            content_type,
            bytes,
            file,
        }) => self::file(content_type, (file.into_file(), bytes), None),
        Ok(Output::Sending {
            content_type,
            file,
            held,
        }) => self::file(content_type, (file.into_file(), held.bytes()), Some(held)),
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
            let mut output = self::file("application/octet-stream", (file, bytes), None);
            output.headers_mut().insert(
                "Alt-Content-Blake3",
                digest.parse().expect("validated digest"),
            );
            output
        }
        Err(error) => store_failure(error, request_key),
    }
}
