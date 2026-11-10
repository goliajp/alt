use super::*;
use golia_platform_contract::{Secret, pkce_challenge};

fn name(origin: &PublicOrigin) -> &'static str {
    if origin.secure() {
        "__Host-alt_login"
    } else {
        "alt_local_login"
    }
}
fn pending_cookie(origin: &PublicOrigin, secret: &str, seconds: u64) -> String {
    format!(
        "{}={secret}; Path=/; HttpOnly; SameSite=Lax; Max-Age={seconds}{}",
        name(origin),
        if origin.secure() { "; Secure" } else { "" }
    )
}
fn cookie(origin: &PublicOrigin, values: &[&str]) -> Result<String> {
    if values.iter().map(|v| v.len()).sum::<usize>() > 8192 {
        return Err(Error::Unauthorized);
    }
    let mut result = None;
    for value in values {
        for item in value.split(';') {
            let (key, value) = item.trim().split_once('=').ok_or(Error::Unauthorized)?;
            if key == name(origin) {
                if result.is_some() {
                    return Err(Error::Unauthorized);
                }
                alt_browser_auth::validate_secret(value).map_err(|_| Error::Unauthorized)?;
                result = Some(value.to_owned());
            }
        }
    }
    result.ok_or(Error::Unauthorized)
}
fn derived(key: &ed25519_dalek::SigningKey, purpose: &'static str, secret: &str) -> String {
    let key = blake3::derive_key(purpose, &key.to_bytes());
    blake3::keyed_hash(&key, secret.as_bytes())
        .to_hex()
        .to_string()
}
fn token(key: &ed25519_dalek::SigningKey, secret: &str) -> String {
    format!(
        "alt1_{}",
        derived(key, "alt Cloud browser credential v1", secret)
    )
}
fn state(key: &ed25519_dalek::SigningKey, secret: &str) -> String {
    derived(key, "alt Cloud browser state v1", secret)
}
fn verifier(key: &ed25519_dalek::SigningKey, secret: &str) -> Secret {
    derived(key, "alt Cloud browser PKCE verifier v1", secret)
        .try_into()
        .expect("derived secret")
}
fn callback(query: Option<&str>) -> Result<(Secret, String)> {
    let query = query.ok_or(Error::Unauthorized)?;
    if query.len() > 1024 {
        return Err(Error::Unauthorized);
    }
    let mut args = std::collections::BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if !matches!(key.as_ref(), "ticket" | "state")
            || args.insert(key.into_owned(), value.into_owned()).is_some()
        {
            return Err(Error::Unauthorized);
        }
    }
    let state = args.remove("state").ok_or(Error::Unauthorized)?;
    alt_browser_auth::validate_secret(&state).map_err(|_| Error::Unauthorized)?;
    let ticket = args
        .remove("ticket")
        .ok_or(Error::Unauthorized)?
        .try_into()
        .map_err(|_| Error::Unauthorized)?;
    Ok((ticket, state))
}
pub(super) async fn handle(
    request: Request<Incoming>,
    state: Arc<State>,
    peer: IpAddr,
) -> Response<ResponseBody> {
    let Some(cloud) = state.factory.cloud.clone() else {
        return store_failure(Error::Unauthorized, None);
    };
    let browser = state.browser.as_ref().expect("browser dispatch").clone();
    if request.method() != hyper::Method::GET
        || request.headers().contains_key(header::AUTHORIZATION)
        || request.headers().contains_key(header::CONTENT_ENCODING)
    {
        return store_failure(Error::Unauthorized, None);
    }
    let starting = request.uri().path() == "/web/login/start";
    let arguments = if starting {
        if request.uri().query().is_some() {
            return store_failure(Error::Unauthorized, None);
        }
        None
    } else {
        let parsed = (|| {
            let (ticket, supplied_state) = callback(request.uri().query())?;
            let secret = cookie(&browser.origin, &headers(request.headers(), "cookie")?)?;
            let expected = self::state(&state.factory.signing_key, &secret);
            if !alt_browser_auth::digest_matches(
                &supplied_state,
                &alt_browser_auth::digest(&expected),
            ) {
                return Err(Error::Unauthorized);
            }
            Ok((ticket, secret))
        })();
        match parsed {
            Ok(values) => Some(values),
            Err(error) => return store_failure(error, None),
        }
    };
    let permit = match state.workers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return failure(
                503,
                ErrorCode::TemporarilyUnavailable,
                "Sign-in service is busy",
                None,
                Retry::SameRequest,
            );
        }
    };
    if !matches!(
        tokio::time::timeout(state.options.body_timeout, spool(request.into_body(), 0)).await,
        Ok(Ok(_))
    ) {
        return store_failure(Error::Unauthorized, None);
    }
    let factory = state.factory.clone();
    let result = tokio::time::timeout(
        state.options.work_timeout,
        crate::http::spawn_blocking(move || {
            let _permit = permit;
            let mut store = factory.open()?;
            store.check_browser_binding(Some(&browser.origin))?;
            if let Some((ticket, secret)) = arguments {
                let id = alt_browser_auth::digest(&secret);
                let ticket_digest = alt_browser_auth::digest(ticket.expose());
                let token = token(&factory.signing_key, &secret);
                match store.claim_cloud_browser_login(&id, &ticket_digest)? {
                    alt_server_store::CloudLoginClaim::Complete => (),
                    alt_server_store::CloudLoginClaim::Exchange {
                        credential_expires_at,
                    } => {
                        let exchange = match cloud.client.exchange(
                            ticket,
                            verifier(&factory.signing_key, &secret),
                            crate::cloud::clock,
                        ) {
                            Ok(exchange) => exchange,
                            Err(alt_cloud_client::Error::Unauthorized) => {
                                store.refuse_cloud_browser_login(&id, &ticket_digest)?;
                                return Err(Error::Unauthorized);
                            }
                            Err(_) => return Err(Error::OutcomeUnknown),
                        };
                        let digest = alt_server_store::cloud_credential_digest(&token)?;
                        let sealed = cloud
                            .sealer
                            .seal(&exchange.grant, &digest)
                            .map_err(|_| Error::Integrity("Cloud grant encryption"))?;
                        store.complete_cloud_browser_login(
                            &id,
                            &ticket_digest,
                            &token,
                            &sealed,
                            &exchange.account,
                            credential_expires_at,
                        )?;
                    }
                }
                let credential = factory.authenticate(&mut store, &token)?;
                let session = store.browser_session(&credential, &browser.origin)?;
                let remaining = session
                    .expires_at_unix_ms
                    .saturating_sub(crate::cloud::clock())
                    / 1000;
                if remaining == 0 {
                    return Err(Error::Unauthorized);
                }
                let session_cookie = browser
                    .origin
                    .set_cookie(&token, remaining.max(60))
                    .map_err(|_| Error::Unauthorized)?;
                Ok((
                    "/".to_owned(),
                    vec![session_cookie, pending_cookie(&browser.origin, "", 0)],
                ))
            } else {
                let secret = alt_browser_auth::secret();
                let local_token = token(&factory.signing_key, &secret);
                store.begin_cloud_browser_login(
                    &alt_browser_auth::digest(&secret),
                    &alt_server_store::cloud_credential_digest(&local_token)?,
                    browser.session_ttl_ms,
                    peer,
                )?;
                let mut destination = url::Url::parse(&format!(
                    "{}/cloud/access",
                    cloud.audience().control_origin.as_str()
                ))
                .map_err(|_| Error::Unauthorized)?;
                destination
                    .query_pairs_mut()
                    .append_pair("installationId", cloud.audience().installation_id.as_str())
                    .append_pair(
                        "challenge",
                        &pkce_challenge(&verifier(&factory.signing_key, &secret)),
                    )
                    .append_pair("state", &self::state(&factory.signing_key, &secret));
                Ok::<_, Error>((
                    destination.to_string(),
                    vec![pending_cookie(&browser.origin, &secret, 300)],
                ))
            }
        }),
    )
    .await;
    match result {
        Ok(Ok(Ok((location, cookies)))) => {
            let mut output = response(
                303,
                "text/plain; charset=utf-8",
                full("Continue signing in"),
            );
            output.headers_mut().insert(
                header::LOCATION,
                location.parse().expect("validated redirect"),
            );
            for cookie in cookies {
                output.headers_mut().append(
                    header::SET_COOKIE,
                    cookie.parse().expect("validated cookie"),
                );
            }
            output
        }
        Ok(Ok(Err(error))) => store_failure(error, None),
        _ => store_failure(Error::OutcomeUnknown, None),
    }
}
