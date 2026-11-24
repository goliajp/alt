//! RFC 8628 token endpoints. Browser consent uses the normal session/CSRF path;
//! these form endpoints admit only the public CLI client, without cookies.
mod exchange;
use super::*;
use alt_remote_protocol::login::*;
use alt_server_store::DevicePoll;
use exchange::{Handover, Leaving, Waiting};
use std::{collections::BTreeMap, io::Read};

/// what a device is refused with, and the seconds after which to ask again
/// where that is known
struct Refused(DeviceError, Option<u64>);

impl From<DeviceError> for Refused {
    fn from(error: DeviceError) -> Self {
        Refused(error, None)
    }
}

pub(super) fn credential(key: &alt_crypto::ed25519::SigningKey, device_digest: &str) -> String {
    let key = alt_hash::blake3::derive_key("alt device credential v1", &key.to_bytes());
    format!(
        "alt1_{}",
        alt_hash::blake3::keyed_hash(&key, device_digest.as_bytes()).to_hex()
    )
}
fn failure(refused: impl Into<Refused>) -> Response<ResponseBody> {
    let Refused(error, again) = refused.into();
    let mut output = response(
        if error == DeviceError::TemporarilyUnavailable {
            503
        } else {
            400
        },
        "application/json",
        full(alt_json::to_vec(&DeviceFailure { error }).expect("device error")),
    );
    if let Some(seconds) = again {
        output
            .headers_mut()
            .insert(header::RETRY_AFTER, seconds.into());
    }
    output
}
fn form(bytes: &[u8]) -> Result<BTreeMap<String, String>, DeviceError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DeviceError::InvalidRequest)?;
    if !text.is_ascii() {
        return Err(DeviceError::InvalidRequest);
    }
    let mut result = BTreeMap::new();
    for (key, value) in alt_url::form::parse(bytes) {
        if result
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(DeviceError::InvalidRequest);
        }
    }
    Ok(result)
}
fn mapped(error: Error) -> Refused {
    match error {
        Error::Unauthorized => DeviceError::AccessDenied.into(),
        Error::Attempts(left) => Refused(
            DeviceError::TemporarilyUnavailable,
            Some(left.div_ceil(1000)),
        ),
        // a transaction that was ended for another is good again at once
        _ => Refused(DeviceError::TemporarilyUnavailable, Some(1)),
    }
}
pub(super) async fn handle(
    request: Request,
    state: Arc<App>,
    peer: std::net::IpAddr,
) -> Response<ResponseBody> {
    let mut result = inner(request, state, peer).await;
    for (name, value) in [
        ("cache-control", "no-store"),
        ("pragma", "no-cache"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
    ] {
        result
            .headers_mut()
            .insert(name, value.parse().expect("static header"));
    }
    result
}
async fn inner(
    request: Request,
    state: Arc<App>,
    peer: std::net::IpAddr,
) -> Response<ResponseBody> {
    let Some(browser) = state.browser.clone() else {
        return failure(DeviceError::InvalidRequest);
    };
    if request.method() != Method::POST
        || request.uri().query().is_some()
        || [
            header::AUTHORIZATION,
            header::COOKIE,
            header::ORIGIN,
            header::CONTENT_ENCODING,
        ]
        .iter()
        .any(|h| request.headers().contains_key(h))
        || request
            .headers()
            .get_all(header::CONTENT_TYPE)
            .iter()
            .count()
            != 1
        || request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::trim)
            != Some("application/x-www-form-urlencoded")
    {
        return failure(DeviceError::InvalidRequest);
    }
    let starting = request.uri().path() == "/oauth/device/code";
    let revoking = request.uri().path() == "/oauth/revoke";
    // nobody is known yet: a body that is slow to come keeps no worker waiting
    let Ok(mut slot) = state.receiving.begin(&receiving::address(peer), 0) else {
        return failure(DeviceError::TemporarilyUnavailable);
    };
    let receipt = body::Receipt::of(&state, Some(&mut slot));
    let Ok(source) = spool(request.into_body(), 2048, receipt).await else {
        return failure(DeviceError::InvalidRequest);
    };
    drop(slot);
    let permit = match state.workers.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return failure(DeviceError::TemporarilyUnavailable),
    };
    let factory = state.factory.clone();
    let waiting = Waiting(Arc::new(Handover::default()));
    let handover = waiting.0.clone();
    let mut work = crate::http::spawn_blocking(move || {
        let _permit = permit;
        let mut source = source;
        let unread = |_| DeviceError::TemporarilyUnavailable;
        source.as_file_mut().rewind().map_err(unread)?;
        let mut bytes = Vec::new();
        source
            .as_file_mut()
            .read_to_end(&mut bytes)
            .map_err(unread)?;
        let mut input = form(&bytes)?;
        if input.remove("client_id").as_deref() != Some(CLI_CLIENT_ID) {
            return Err(DeviceError::InvalidClient.into());
        }
        let mut store = factory.open().map_err(mapped)?;
        let bound = store.check_browser_binding(Some(&browser.origin));
        bound.map_err(mapped)?;
        if revoking {
            let token = input.remove("token").ok_or(DeviceError::InvalidRequest)?;
            let secret = token.strip_prefix("alt1_");
            if !input.is_empty() || secret.is_none_or(|s| device_secret(s).is_err()) {
                return Err(DeviceError::InvalidRequest.into());
            }
            store.revoke_client_token(&token).map_err(mapped)?;
            Ok(alt_json::json!({}))
        } else if starting {
            begin(&factory, &mut store, &browser.origin, input, peer)
        } else {
            if input.remove("grant_type").as_deref() != Some(DEVICE_GRANT_TYPE) {
                return Err(DeviceError::UnsupportedGrantType.into());
            }
            let code = input.remove("device_code");
            let code = code.ok_or(DeviceError::InvalidRequest)?;
            if !input.is_empty() || device_secret(&code).is_err() {
                return Err(DeviceError::InvalidRequest.into());
            }
            let digest = alt_browser_auth::digest(&code);
            exchange::exchange(&factory, &mut store, &digest, &handover)
        }
    });
    let outcome = match tokio::time::timeout(state.options.work_timeout, &mut work).await {
        Ok(outcome) => outcome,
        Err(_) => match waiting.0.leave() {
            Leaving::Unspent => return failure(DeviceError::TemporarilyUnavailable),
            // its token is revoked by the worker: the device begins again
            Leaving::Withdrawn => return failure(DeviceError::ExpiredToken),
            Leaving::Answered => work.await,
        },
    };
    match outcome {
        Ok(Ok(value)) => response(
            200,
            "application/json",
            full(alt_json::to_vec(&value).expect("device response")),
        ),
        Ok(Err(refused)) => failure(refused),
        Err(_) => failure(DeviceError::TemporarilyUnavailable),
    }
}

/// A code begun for a device that asks for one, and what the device is told of it.
fn begin(
    factory: &StoreFactory,
    store: &mut Store,
    origin: &alt_browser_auth::PublicOrigin,
    mut input: BTreeMap<String, String>,
    peer: std::net::IpAddr,
) -> Result<alt_json::Value, Refused> {
    if input.remove("scope").as_deref() != Some(CLI_SCOPE) {
        return Err(DeviceError::InvalidScope.into());
    }
    if !input.is_empty() {
        return Err(DeviceError::InvalidRequest.into());
    }
    let code = alt_browser_auth::secret();
    let digest = alt_browser_auth::digest(&code);
    let mut random = [0u8; 8];
    alt_os::random::fill(&mut random);
    let human: String = random
        .iter()
        .map(|b| USER_CODE_ALPHABET[(b & 31) as usize] as char)
        .collect();
    let human = user_code(&human).expect("random human code");
    let token = credential(&factory.signing_key, &digest);
    let token = alt_server_store::cloud_credential_digest(&token).map_err(mapped)?;
    let begun = store.begin_device_authorization(origin, &digest, &human, &token, peer);
    begun.map_err(mapped)?;
    let uri = format!("{}/#device", origin.as_str());
    alt_json::to_value(DeviceAuthorizationResponse {
        device_code: code,
        user_code: human.clone(),
        verification_uri: uri.clone(),
        verification_uri_complete: format!("{uri}?user_code={human}"),
        expires_in: DEVICE_LIFETIME_MS / 1000,
        interval: DEVICE_INTERVAL_MS / 1000,
    })
    .map_err(|_| DeviceError::TemporarilyUnavailable.into())
}
