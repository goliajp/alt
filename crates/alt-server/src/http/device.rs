//! RFC 8628 token endpoints. Browser consent uses the normal session/CSRF path;
//! these form endpoints admit only the public CLI client, without cookies.
use super::*;
use alt_remote_protocol::login::*;
use alt_server_store::DevicePoll;
use std::{collections::BTreeMap, io::Read};

pub(super) fn credential(key: &ed25519_dalek::SigningKey, device_digest: &str) -> String {
    let key = alt_hash::blake3::derive_key("alt device credential v1", &key.to_bytes());
    format!(
        "alt1_{}",
        alt_hash::blake3::keyed_hash(&key, device_digest.as_bytes()).to_hex()
    )
}
fn failure(error: DeviceError) -> Response<ResponseBody> {
    response(
        if error == DeviceError::TemporarilyUnavailable {
            503
        } else {
            400
        },
        "application/json",
        full(serde_json::to_vec(&DeviceFailure { error }).expect("device error")),
    )
}
fn form(bytes: &[u8]) -> Result<BTreeMap<String, String>, DeviceError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DeviceError::InvalidRequest)?;
    if !text.is_ascii() {
        return Err(DeviceError::InvalidRequest);
    }
    let mut result = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(bytes) {
        if result
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(DeviceError::InvalidRequest);
        }
    }
    Ok(result)
}
fn mapped(error: Error) -> DeviceError {
    match error {
        Error::Unauthorized => DeviceError::AccessDenied,
        _ => DeviceError::TemporarilyUnavailable,
    }
}
pub(super) async fn handle(
    request: Request<Incoming>,
    state: Arc<State>,
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
    request: Request<Incoming>,
    state: Arc<State>,
    peer: std::net::IpAddr,
) -> Response<ResponseBody> {
    let Some(browser) = state.browser.clone() else {
        return failure(DeviceError::InvalidRequest);
    };
    if request.method() != hyper::Method::POST
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
    let permit = match state.workers.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return failure(DeviceError::TemporarilyUnavailable),
    };
    let source =
        match tokio::time::timeout(state.options.body_timeout, spool(request.into_body(), 2048))
            .await
        {
            Ok(Ok(source)) => source,
            _ => return failure(DeviceError::InvalidRequest),
        };
    let factory = state.factory.clone();
    let outcome = tokio::time::timeout(
        state.options.work_timeout,
        crate::http::spawn_blocking(move || {
            let _permit = permit;
            let mut source = source;
            source
                .as_file_mut()
                .rewind()
                .map_err(|_| DeviceError::TemporarilyUnavailable)?;
            let mut bytes = Vec::new();
            source
                .as_file_mut()
                .read_to_end(&mut bytes)
                .map_err(|_| DeviceError::TemporarilyUnavailable)?;
            let mut input = form(&bytes)?;
            if input.remove("client_id").as_deref() != Some(CLI_CLIENT_ID) {
                return Err(DeviceError::InvalidClient);
            }
            let mut store = factory.open().map_err(mapped)?;
            store
                .check_browser_binding(Some(&browser.origin))
                .map_err(mapped)?;
            if revoking {
                let token = input.remove("token").ok_or(DeviceError::InvalidRequest)?;
                if !input.is_empty()
                    || token
                        .strip_prefix("alt1_")
                        .is_none_or(|s| device_secret(s).is_err())
                {
                    return Err(DeviceError::InvalidRequest);
                }
                store.revoke_client_token(&token).map_err(mapped)?;
                Ok(serde_json::json!({}))
            } else if starting {
                if input.remove("scope").as_deref() != Some(CLI_SCOPE) {
                    return Err(DeviceError::InvalidScope);
                }
                if !input.is_empty() {
                    return Err(DeviceError::InvalidRequest);
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
                store
                    .begin_device_authorization(
                        &browser.origin,
                        &digest,
                        &human,
                        &alt_server_store::cloud_credential_digest(&token).map_err(mapped)?,
                        peer,
                    )
                    .map_err(mapped)?;
                let uri = format!("{}/#device", browser.origin.as_str());
                serde_json::to_value(DeviceAuthorizationResponse {
                    device_code: code,
                    user_code: human.clone(),
                    verification_uri: uri.clone(),
                    verification_uri_complete: format!("{uri}?user_code={human}"),
                    expires_in: DEVICE_LIFETIME_MS / 1000,
                    interval: DEVICE_INTERVAL_MS / 1000,
                })
                .map_err(|_| DeviceError::TemporarilyUnavailable)
            } else {
                if input.remove("grant_type").as_deref() != Some(DEVICE_GRANT_TYPE) {
                    return Err(DeviceError::UnsupportedGrantType);
                }
                let code = input
                    .remove("device_code")
                    .ok_or(DeviceError::InvalidRequest)?;
                if !input.is_empty() || device_secret(&code).is_err() {
                    return Err(DeviceError::InvalidRequest);
                }
                let digest = alt_browser_auth::digest(&code);
                match store.poll_device_authorization(&digest).map_err(mapped)? {
                    DevicePoll::Waiting(error) => Err(error),
                    DevicePoll::Ready { expires_at_unix_ms } => {
                        let token = credential(&factory.signing_key, &digest);
                        factory.authenticate(&mut store, &token).map_err(mapped)?;
                        let remaining =
                            expires_at_unix_ms.saturating_sub(crate::cloud::clock()) / 1000;
                        if remaining == 0 {
                            return Err(DeviceError::ExpiredToken);
                        }
                        serde_json::to_value(DeviceTokenResponse {
                            access_token: token,
                            token_type: "Bearer".into(),
                            expires_in: remaining,
                            scope: CLI_SCOPE.into(),
                        })
                        .map_err(|_| DeviceError::TemporarilyUnavailable)
                    }
                }
            }
        }),
    )
    .await;
    match outcome {
        Ok(Ok(Ok(value))) => response(
            200,
            "application/json",
            full(serde_json::to_vec(&value).expect("device response")),
        ),
        Ok(Ok(Err(error))) => failure(error),
        _ => failure(DeviceError::TemporarilyUnavailable),
    }
}
