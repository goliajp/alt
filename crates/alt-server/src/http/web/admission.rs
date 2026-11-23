//! What a browser request waits for before it runs: its body and a worker.
use super::*;
use tokio::sync::OwnedSemaphorePermit;

/// the response to a request that is not gone on with
type Refusal = Box<Response<ResponseBody>>;

fn busy(message: &str) -> Refusal {
    Box::new(failure(
        503,
        ErrorCode::TemporarilyUnavailable,
        message,
        None,
        Retry::SameRequest,
    ))
}

/// The worker a request runs on and, where it hashes a password, one of the two
/// that do.
///
/// An unauthenticated password flood cannot allocate one Argon2 arena per
/// HTTP connection, nor starve all normal publication workers.
pub(super) fn admit(
    state: &App,
    password: bool,
) -> std::result::Result<(OwnedSemaphorePermit, Option<OwnedSemaphorePermit>), Refusal> {
    let worker = state.workers.clone().try_acquire_owned();
    let worker = worker.map_err(|_| busy("Instance is busy"))?;
    if !password {
        return Ok((worker, None));
    }
    let hashing = state.passwords.clone().try_acquire_owned();
    let hashing = hashing.map_err(|_| busy("Sign-in service is busy"))?;
    Ok((worker, Some(hashing)))
}

/// The body of a request, of `max` bytes at most and within the time a body has.
pub(super) async fn receive(
    state: &App,
    body: Body,
    max: u64,
) -> std::result::Result<alt_os::temp::NamedTempFile, Refusal> {
    match tokio::time::timeout(state.options.body_timeout, spool(body, max)).await {
        Ok(Ok(source)) => Ok(source),
        Ok(Err(error)) => Err(unreceived(Some(error))),
        Err(_) => Err(unreceived(None)),
    }
}

/// the response where the authority of a migration request could not be checked in time
pub(super) fn unchecked() -> Refusal {
    busy("Current migration authority could not be checked")
}

/// the response to a body that could not be received; no error means it ran out of time
fn unreceived(error: Option<BodyError>) -> Refusal {
    match error {
        Some(BodyError::Limit) => Box::new(failure(
            413,
            ErrorCode::QuotaExceeded,
            "Request body is too large",
            None,
            Retry::Never,
        )),
        Some(BodyError::Malformed) => Box::new(failure(
            400,
            ErrorCode::InvalidRequest,
            "Incomplete request body",
            None,
            Retry::Never,
        )),
        _ => busy("Request could not be read"),
    }
}
