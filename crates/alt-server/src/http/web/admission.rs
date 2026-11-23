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

/// The worker a request of `caller` runs on and, where it hashes a password, one
/// of the two places among those that do, which it waits for.
///
/// An unauthenticated password flood cannot allocate one Argon2 arena per
/// HTTP connection, nor starve all normal publication workers.
async fn admit(
    state: &App,
    caller: &str,
    password: bool,
) -> std::result::Result<(OwnedSemaphorePermit, Option<OwnedSemaphorePermit>), Refusal> {
    let hashing = match password {
        true => {
            let place = state.passwords.place(caller, state.options.work_timeout);
            Some(place.await.ok_or_else(|| busy("Sign-in service is busy"))?)
        }
        false => None,
    };
    let worker = state.workers.clone().try_acquire_owned();
    let worker = worker.map_err(|_| busy("Instance is busy"))?;
    Ok((worker, hashing))
}

/// The body of a request of `caller`, of `max` bytes at most and within the time a
/// body has, and the place it has among the bodies being received: kept by a
/// request that goes on to wait for a worker, until it has one. A request that
/// brings no body takes no place.
async fn receive(
    state: &App,
    caller: &str,
    body: Body,
    max: u64,
) -> std::result::Result<(alt_os::temp::NamedTempFile, Option<receiving::Slot>), Refusal> {
    if max == 0 || axum::body::HttpBody::is_end_stream(&body) {
        let receipt = body::Receipt::of(state, None);
        let source = spool(body, max, receipt).await.map_err(unreceived)?;
        return Ok((source, None));
    }
    let begun = state.receiving.begin(caller, 0);
    let mut slot = begun.map_err(|full| Box::new(receiving::refusal(full, None)))?;
    let receipt = body::Receipt::of(state, Some(&mut slot));
    let source = spool(body, max, receipt).await.map_err(unreceived)?;
    Ok((source, Some(slot)))
}

/// the response where the authority of a migration request could not be checked in time
fn unchecked() -> Refusal {
    busy("Current migration authority could not be checked")
}

/// the response to a body that could not be received
fn unreceived(error: BodyError) -> Refusal {
    match error {
        BodyError::Limit => Box::new(failure(
            413,
            ErrorCode::QuotaExceeded,
            "Request body is too large",
            None,
            Retry::Never,
        )),
        BodyError::Malformed => Box::new(failure(
            400,
            ErrorCode::InvalidRequest,
            "Incomplete request body",
            None,
            Retry::Never,
        )),
        _ => busy("Request could not be read"),
    }
}

/// What a browser request runs with: its body, the worker, and where it hashes a
/// password one of the two that do. Nobody is known while the body of most
/// requests comes, so it is received before a worker is taken. A migration is
/// authorized first, on a worker it gives back while its body comes.
pub(super) async fn admitted(
    state: &App,
    browser: &WebOptions,
    route: &WebRoute,
    token: &Option<String>,
    peer: IpAddr,
    body: Body,
    writing: bool,
) -> std::result::Result<Admitted, Refusal> {
    let caller = receiving::address(peer);
    let password = matches!(route, WebRoute::Login | WebRoute::CreateUser);
    let WebRoute::Portable(selected) = route else {
        let small = match writing {
            true => state.factory.limits.request_bytes.min(16_384) as u64,
            false => 0,
        };
        let (source, _) = receive(state, &caller, body, small).await?;
        let (permit, hashing) = admit(state, &caller, password).await?;
        return Ok((source, permit, hashing));
    };
    let (permit, hashing) = admit(state, &caller, password).await?;
    let bound = portable_bound(state, browser, selected.clone(), token.clone(), permit);
    let max = match bound.await {
        Ok(max) => max,
        Err(Some(error)) => return Err(Box::new(store_failure(error, None))),
        Err(None) => return Err(unchecked()),
    };
    // its place is kept while it waits: what waits for a worker with its
    // body whole counts among the bodies held like one that still comes
    let (source, slot) = receive(state, &caller, body, max).await?;
    let permit = receiving::worker(state, None).await?;
    drop(slot);
    Ok((source, permit, hashing))
}

/// the body of a request, its worker and its place among those that hash passwords
type Admitted = (
    alt_os::temp::NamedTempFile,
    OwnedSemaphorePermit,
    Option<OwnedSemaphorePermit>,
);

/// the body bound of a migration request, once its caller is authorized on the
/// worker `permit` is of; no error means the check did not finish
async fn portable_bound(
    state: &App,
    browser: &WebOptions,
    selected: crate::PortableRoute,
    token: Option<String>,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> std::result::Result<u64, Option<Error>> {
    let factory = state.factory.clone();
    let token = token.ok_or(Error::Unauthorized);
    let origin = browser.origin.clone();
    let options = state.options.native.clone();
    let prepared = tokio::time::timeout(
        state.options.work_timeout,
        crate::http::spawn_observed(Phase::Execute, move || {
            let mut store = factory.open()?;
            let token = token?;
            let credential = factory.authenticate(&mut store, &token)?;
            store.browser_session(&credential, &origin)?;
            let _permit = permit;
            let max = crate::portable::request_bytes(&mut store, &credential, &options, &selected)?;
            Ok::<_, Error>(max)
        }),
    )
    .await;
    match prepared {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => Err(Some(error)),
        _ => Err(None),
    }
}
