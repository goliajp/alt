mod cloud_login;
mod execute;
mod input;
use super::*;
use crate::web_route::{self, WebRoute};
use alt_browser_auth::PublicOrigin;
use alt_server_store::Result;
use std::net::IpAddr;

#[derive(Clone)]
pub struct WebOptions {
    pub origin: PublicOrigin,
    pub assets: crate::WebAssets,
    pub session_ttl_ms: u64,
}
impl WebOptions {
    pub(super) fn validate(&self) -> std::io::Result<()> {
        if !(60_000..=43_200_000).contains(&self.session_ttl_ms) {
            return Err(std::io::Error::other("invalid browser session lifetime"));
        }
        Ok(())
    }
}
fn csrf(token: &str, key: &alt_crypto::ed25519::SigningKey) -> String {
    let secret = alt_hash::blake3::derive_key("alt browser CSRF key v1", &key.to_bytes());
    alt_hash::blake3::keyed_hash(&secret, token.as_bytes())
        .to_hex()
        .to_string()
}
fn headers<'a>(values: &'a HeaderMap, name: &str) -> Result<Vec<&'a str>> {
    values
        .get_all(name)
        .iter()
        .map(|v| v.to_str().map_err(|_| Error::Unauthorized))
        .collect()
}
fn secure(mut value: Response<ResponseBody>) -> Response<ResponseBody> {
    value.headers_mut().insert("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    value
        .headers_mut()
        .insert("Referrer-Policy", "no-referrer".parse().unwrap());
    value
        .headers_mut()
        .insert("X-Frame-Options", "DENY".parse().unwrap());
    value.headers_mut().insert(
        "Cross-Origin-Resource-Policy",
        "same-origin".parse().unwrap(),
    );
    value
}
fn unauthenticated() -> Response<ResponseBody> {
    let mut value = failure(
        401,
        ErrorCode::Unauthenticated,
        "Sign in to this instance",
        None,
        Retry::Never,
    );
    value.headers_mut().remove(header::WWW_AUTHENTICATE);
    value
}
pub(super) async fn handle(
    request: Request,
    state: Arc<App>,
    peer: IpAddr,
) -> Response<ResponseBody> {
    secure(handle_inner(request, state, peer).await)
}
/// a file of the browser bundle; the caller found one under the request path
pub(super) fn asset(state: &App, request: &Request) -> Response<ResponseBody> {
    let found = if request.method() == Method::GET && request.uri().query().is_none() {
        let browser = state.browser.as_ref().expect("browser dispatch");
        browser.assets.get(request.uri().path())
    } else {
        None
    };
    secure(match found {
        Some(asset) => response(
            200,
            asset.content_type,
            full(Bytes::copy_from_slice(&asset.bytes)),
        ),
        None => failure(
            404,
            ErrorCode::InvalidRequest,
            "Unknown resource",
            None,
            Retry::Never,
        ),
    })
}
async fn handle_inner(request: Request, state: Arc<App>, peer: IpAddr) -> Response<ResponseBody> {
    let browser = state.browser.as_ref().expect("browser dispatch").clone();
    if matches!(
        request.uri().path(),
        "/web/login/start" | "/web/login/callback"
    ) {
        return cloud_login::handle(request, state, peer).await;
    }
    let route = match web_route::parse(
        request.method().as_str(),
        request.uri().path(),
        request.uri().query(),
        &state.factory.instance.installation_id,
    ) {
        Ok(route) => route,
        Err(error) => return store_failure(error, None),
    };
    observation::web(&route);
    let writing = matches!(*request.method(), Method::POST | Method::PUT);
    let binary_upload = matches!(
        &route,
        WebRoute::Portable(crate::PortableRoute::Import(
            _,
            crate::ImportAction::Upload(_)
        ))
    );
    let archive_download = matches!(
        &route,
        WebRoute::Portable(crate::PortableRoute::Export(
            _,
            _,
            crate::ExportAction::Archive
        ))
    );
    let expected_content_type = if binary_upload {
        "application/octet-stream"
    } else {
        "application/json"
    };
    if request.headers().contains_key(header::AUTHORIZATION) {
        return unauthenticated();
    }
    // Neither bearer credentials nor proxy headers can become browser identity.
    let token = match headers(request.headers(), "cookie").and_then(|values| {
        browser
            .origin
            .session_cookie(&values)
            .map_err(|_| Error::Unauthorized)
    }) {
        Ok(token) => token,
        Err(_) => return unauthenticated(),
    };
    let login = route == WebRoute::Login;
    let session_request = route == WebRoute::Session;
    if writing {
        let checked = (|| {
            let origins = headers(request.headers(), "origin")?;
            if login {
                browser
                    .origin
                    .check_origin(&origins)
                    .map_err(|_| Error::Unauthorized)
            } else {
                let token = token.as_deref().ok_or(Error::Unauthorized)?;
                let proofs = headers(request.headers(), "x-alt-csrf")?;
                browser
                    .origin
                    .check_mutation(
                        &origins,
                        &proofs,
                        &alt_browser_auth::digest(&csrf(token, &state.factory.signing_key)),
                    )
                    .map_err(|_| Error::Unauthorized)
            }
        })();
        if checked.is_err() {
            return store_failure(Error::Unauthorized, None);
        }
    }
    if !matches!(route, WebRoute::Login | WebRoute::Config) && token.is_none() {
        return unauthenticated();
    }
    if request.headers().contains_key(header::CONTENT_ENCODING)
        || writing
            && (request
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
                    .is_none_or(|v| !v.trim().eq_ignore_ascii_case(expected_content_type)))
    {
        return failure(
            415,
            ErrorCode::InvalidRequest,
            "Use the required content type without content encoding",
            None,
            Retry::Never,
        );
    }
    let permit = match state.workers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return failure(
                503,
                ErrorCode::TemporarilyUnavailable,
                "Instance is busy",
                None,
                Retry::SameRequest,
            );
        }
    };
    // An unauthenticated password flood cannot allocate one Argon2 arena per
    // HTTP connection, nor starve all normal publication workers.
    let password_permit = if matches!(route, WebRoute::Login | WebRoute::CreateUser) {
        match state.passwords.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                return failure(
                    503,
                    ErrorCode::TemporarilyUnavailable,
                    "Sign-in service is busy",
                    None,
                    Retry::SameRequest,
                );
            }
        }
    } else {
        None
    };
    let (permit, max) = if let WebRoute::Portable(selected) = &route {
        match portable_bound(&state, &browser, selected.clone(), token.clone(), permit).await {
            Ok(value) => value,
            Err(Some(error)) => return store_failure(error, None),
            Err(None) => {
                return failure(
                    503,
                    ErrorCode::TemporarilyUnavailable,
                    "Current migration authority could not be checked",
                    None,
                    Retry::SameRequest,
                );
            }
        }
    } else {
        (
            permit,
            if writing {
                state.factory.limits.request_bytes.min(16_384) as u64
            } else {
                0
            },
        )
    };
    let source =
        match tokio::time::timeout(state.options.body_timeout, spool(request.into_body(), max))
            .await
        {
            Ok(Ok(source)) => source,
            Ok(Err(error)) => return unreceived(Some(error)),
            Err(_) => return unreceived(None),
        };
    let call = execute::Call {
        factory: state.factory.clone(),
        options: state.options.native.clone(),
        browser,
        token,
        peer,
    };
    let outcome = tokio::time::timeout(
        state.options.work_timeout,
        crate::http::spawn_observed(Phase::Execute, move || {
            let _permit = permit;
            let _password_permit = password_permit;
            call.run(route, source)
        }),
    )
    .await;
    match outcome {
        Ok(Ok(Ok((output, cookie)))) => {
            let download = matches!(output, Output::File { .. } | Output::Temporary { .. });
            let mut response = render_output(Ok(output), None);
            if let Some(cookie) = cookie {
                response.headers_mut().insert(
                    header::SET_COOKIE,
                    cookie.parse().expect("validated cookie"),
                );
            }
            if download {
                response.headers_mut().insert(
                    header::CONTENT_DISPOSITION,
                    if archive_download {
                        "attachment; filename=\"alt-snapshot.altportable\""
                    } else {
                        "attachment; filename=\"alt-original.bin\""
                    }
                    .parse()
                    .unwrap(),
                );
            }
            response
        }
        Ok(Ok(Err(Error::Unauthorized))) if login || session_request => unauthenticated(),
        Ok(Ok(Err(error))) => store_failure(error, None),
        _ => failure(
            503,
            if writing {
                ErrorCode::OutcomeUnknown
            } else {
                ErrorCode::TemporarilyUnavailable
            },
            "The response is unavailable; inspect the current state before retrying a change",
            None,
            if writing {
                Retry::QueryOutcome
            } else {
                Retry::SameRequest
            },
        ),
    }
}
/// the worker permit and the body bound of a migration request, once its caller is
/// authorized; no error means the check did not finish
async fn portable_bound(
    state: &App,
    browser: &WebOptions,
    selected: crate::PortableRoute,
    token: Option<String>,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> std::result::Result<(tokio::sync::OwnedSemaphorePermit, u64), Option<Error>> {
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
            let max = crate::portable::request_bytes(&mut store, &credential, &options, &selected)?;
            Ok::<_, Error>((permit, max))
        }),
    )
    .await;
    match prepared {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => Err(Some(error)),
        _ => Err(None),
    }
}
/// the response to a body that could not be received; no error means it ran out of time
fn unreceived(error: Option<BodyError>) -> Response<ResponseBody> {
    match error {
        Some(BodyError::Limit) => failure(
            413,
            ErrorCode::QuotaExceeded,
            "Request body is too large",
            None,
            Retry::Never,
        ),
        Some(BodyError::Malformed) => failure(
            400,
            ErrorCode::InvalidRequest,
            "Incomplete request body",
            None,
            Retry::Never,
        ),
        _ => failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "Request could not be read",
            None,
            Retry::SameRequest,
        ),
    }
}
