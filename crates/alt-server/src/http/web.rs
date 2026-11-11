mod cloud_login;
use super::*;
use crate::web_route::{self, WebRoute};
use alt_browser_auth::PublicOrigin;
use alt_server_store::{Credential, Result, Store};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io::Read, net::IpAddr};

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
fn headers<'a>(values: &'a hyper::HeaderMap, name: &str) -> Result<Vec<&'a str>> {
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
    request: Request<Incoming>,
    state: Arc<State>,
    peer: IpAddr,
) -> Response<ResponseBody> {
    secure(handle_inner(request, state, peer).await)
}
async fn handle_inner(
    request: Request<Incoming>,
    state: Arc<State>,
    peer: IpAddr,
) -> Response<ResponseBody> {
    let browser = state.browser.as_ref().expect("browser dispatch").clone();
    if !request.uri().path().starts_with("/web/") {
        if request.method() != hyper::Method::GET || request.uri().query().is_some() {
            return failure(
                404,
                ErrorCode::InvalidRequest,
                "Unknown resource",
                None,
                Retry::Never,
            );
        }
        let Some(asset) = browser.assets.get(request.uri().path()) else {
            return failure(
                404,
                ErrorCode::InvalidRequest,
                "Unknown resource",
                None,
                Retry::Never,
            );
        };
        return response(
            200,
            asset.content_type,
            full(Bytes::copy_from_slice(&asset.bytes)),
        );
    }
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
    let writing = matches!(*request.method(), hyper::Method::POST | hyper::Method::PUT);
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
        let selected = selected.clone();
        let factory = state.factory.clone();
        let token = token.clone().ok_or(Error::Unauthorized);
        let origin = browser.origin.clone();
        let options = state.options.native.clone();
        let prepared = tokio::time::timeout(
            state.options.work_timeout,
            crate::http::spawn_observed(Phase::Execute, move || {
                let mut store = factory.open()?;
                let token = token?;
                let credential = factory.authenticate(&mut store, &token)?;
                store.browser_session(&credential, &origin)?;
                let max =
                    crate::portable::request_bytes(&mut store, &credential, &options, &selected)?;
                Ok::<_, Error>((permit, max))
            }),
        )
        .await;
        match prepared {
            Ok(Ok(Ok(value))) => value,
            Ok(Ok(Err(error))) => return store_failure(error, None),
            _ => {
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
            Ok(Err(BodyError::Limit)) => {
                return failure(
                    413,
                    ErrorCode::QuotaExceeded,
                    "Request body is too large",
                    None,
                    Retry::Never,
                );
            }
            Ok(Err(BodyError::Malformed)) => {
                return failure(
                    400,
                    ErrorCode::InvalidRequest,
                    "Incomplete request body",
                    None,
                    Retry::Never,
                );
            }
            _ => {
                return failure(
                    503,
                    ErrorCode::TemporarilyUnavailable,
                    "Request could not be read",
                    None,
                    Retry::SameRequest,
                );
            }
        };
    let factory = state.factory.clone();
    let options = state.options.native.clone();
    let outcome = tokio::time::timeout(state.options.work_timeout, crate::http::spawn_observed(Phase::Execute, move || {
        let _permit = permit;
        let _password_permit = password_permit;
        let mut source = source;
        source.as_file_mut().rewind()?;
        let mut store = factory.open()?;
        store.check_browser_binding(Some(&browser.origin))?;
        if let WebRoute::Portable(selected) = &route {
            let token = token.as_deref().ok_or(Error::Unauthorized)?;
            let credential = factory.authenticate(&mut store, token)?;
            store.browser_session(&credential, &browser.origin)?;
            let output = crate::portable::execute(&mut store, &credential, &options, selected.clone(), source.as_file_mut(), &mut |store| {
                let current = factory.authenticate(store, token)?;
                store.browser_session(&current, &browser.origin)?;
                Ok(current)
            })?;
            return Ok((output, None));
        }
        let mut raw = Vec::new();
        source.as_file_mut().read_to_end(&mut raw)?;
        if route == WebRoute::Config {
            return Ok((Output::Json(json!({"deployment_mode":store.spec().deployment_mode,"workspace_id":store.spec().workspace_id})), None));
        }
        if login {
            let input: Login = serde_json::from_slice(&raw)?;
            let authenticated = store.browser_login(&browser.origin, &input.login_name, &input.password, peer, browser.session_ttl_ms)?.ok_or(Error::Unauthorized)?;
            let credential = store.authenticate(&authenticated.token)?;
            let session = session(&mut store, &credential, &browser.origin, &authenticated.token, &factory.signing_key)?;
            let cookie = browser.origin.set_cookie(&authenticated.token, browser.session_ttl_ms / 1000).map_err(|_| Error::Integrity("session cookie"))?;
            return Ok((Output::Json(session), Some(cookie)));
        }
        let token = token.ok_or(Error::Unauthorized)?;
        let credential = factory.authenticate(&mut store, &token)?;
        // Repeat membership/account checks for every browser operation. Business
        // methods additionally recheck the credential inside their transaction.
        store.browser_session(&credential, &browser.origin)?;
        match route {
            WebRoute::ChangeLink(selected) => {
                crate::change_links::api::request_bytes(&mut store, &credential, &selected)?;
                let output = crate::change_links::api::execute(&mut store, &credential, &factory.change_links, selected, &mut &raw[..])?;
                Ok((output, None))
            }
            WebRoute::Session => Ok((Output::Json(session(&mut store, &credential, &browser.origin, &token, &factory.signing_key)?), None)),
            WebRoute::Navigation => {
                let cloud=factory.cloud.as_ref().ok_or(Error::Unauthorized)?;
                Ok((output(cloud.navigation(&mut store,&token)?)?,None))
            }
            WebRoute::Sessions => Ok((output(store.browser_sessions(&credential, &browser.origin)?)?, None)),
            WebRoute::DeviceConsent(code) => Ok((output(store.device_consent(&credential,&browser.origin,&code,peer)?)?,None)),
            WebRoute::DeviceDecision => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Decision { user_code:String, allow:bool }
                let input:Decision=serde_json::from_slice(&raw)?;
                let consent=store.device_consent(&credential,&browser.origin,&input.user_code,peer)?;
                let local=super::device::credential(&factory.signing_key,&consent.id);
                let sealed=if input.allow {
                    if let Some(cloud)=&factory.cloud {
                        let descriptor=store.cloud_refresh(&token)?;
                        let grant=cloud.sealer.open(&descriptor.sealed_grant,&descriptor.credential_digest).map_err(|_|Error::Unauthorized)?;
                        Some(cloud.sealer.seal(&grant,&alt_server_store::cloud_credential_digest(&local)?).map_err(|_|Error::Integrity("Cloud grant encryption"))?)
                    } else {None}
                } else {None};
                Ok((output(store.decide_device_authorization(&credential,&browser.origin,&input.user_code,input.allow,&local,sealed.as_deref())?)?,None))
            },
            WebRoute::AgentDelegations(after) => Ok((output(store.agent_delegations(&credential,&after)?)?,None)),
            WebRoute::RevokeAgent(id) => {
                let _:Empty=serde_json::from_slice(&raw)?;
                store.revoke_agent(&credential,&id)?;
                Ok((Output::Json(json!({"revoked":true})),None))
            },
            WebRoute::ClientCredentials => Ok((output(store.client_credentials(&credential)?)?,None)),
            WebRoute::RevokeClientCredential(id) => {
                let _:Empty=serde_json::from_slice(&raw)?;
                store.revoke_client_credential(&credential,&id)?;
                Ok((Output::Json(json!({"revoked":true})),None))
            },
            WebRoute::RevokeSession(id) => {
                let _: Empty = serde_json::from_slice(&raw)?;
                store.revoke_browser_session(&credential, &browser.origin, &id)?;
                Ok((Output::Json(json!({"revoked":true})), None))
            },
            WebRoute::Logout => {
                let _: Empty = serde_json::from_slice(&raw)?;
                store.browser_logout(&credential, &browser.origin)?;
                Ok((Output::Json(json!({"signed_out":true})), Some(browser.origin.clear_cookie())))
            },
            route => execute(&mut store, &credential, &options, route, &raw).map(|out| (out, None)),
        }
    })).await;
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    login_name: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn session(
    store: &mut Store,
    credential: &Credential,
    origin: &PublicOrigin,
    token: &str,
    key: &alt_crypto::ed25519::SigningKey,
) -> Result<Value> {
    let session = store.browser_session(credential, origin)?;
    let spec = store.spec();
    Ok(
        json!({"installation_id":spec.installation_id,"workspace_id":spec.workspace_id,"deployment_mode":spec.deployment_mode,"runtime_generation":spec.runtime_generation.to_string(),"build_id":option_env!("ALT_BUILD_ID").unwrap_or("development"),"user":session.user,"csrf":csrf(token,key),"products":[{"product":"alt","title":"alt","url":"/"}]}),
    )
}
fn output(value: impl serde::Serialize) -> Result<Output> {
    Ok(Output::Json(serde_json::to_value(value)?))
}
fn execute(
    store: &mut Store,
    credential: &Credential,
    options: &NativeOptions,
    route: WebRoute,
    raw: &[u8],
) -> Result<Output> {
    match route {
        WebRoute::InstanceStatus => output(store.instance_status(credential)?),
        WebRoute::InstanceAudit { before } => output(store.instance_audit(credential, before, 50)?),
        WebRoute::Repositories { after } => {
            output(store.workbench_repositories(credential, &after, 50)?)
        }
        WebRoute::Inbox { after } => output(store.workbench_inbox(credential, &after, 50)?),
        WebRoute::Repository(repository) => {
            let info = store.repository_info(credential, &repository)?;
            output(
                json!({"scope":info.scope,"default_branch":info.default_branch,"refs":info.refs}),
            )
        }
        WebRoute::History(repository, after) => output(crate::browse::history(
            store,
            credential,
            &repository,
            after,
            40,
        )?),
        WebRoute::Tree(repository, commit, path) => output(crate::browse::tree(
            store,
            credential,
            &repository,
            commit,
            &path,
        )?),
        WebRoute::Object(repository, oid, false) => {
            output(crate::browse::blob(store, credential, &repository, oid)?)
        }
        WebRoute::Object(repository, oid, true) => {
            let (_, bytes, mut file) = store.open_published_object(credential, &repository, oid)?;
            let mut copy = alt_os::temp::NamedTempFile::new()?;
            std::io::copy(&mut file, copy.as_file_mut())?;
            copy.as_file_mut().rewind()?;
            Ok(Output::Temporary {
                content_type: "application/octet-stream",
                bytes,
                file: copy,
            })
        }
        WebRoute::Content(repository, digest) => {
            let scope = store.repository_info(credential, &repository)?.scope;
            let (bytes, file) = store.open_published_content(credential, &scope, &digest)?;
            Ok(Output::File {
                bytes,
                digest,
                file,
            })
        }
        WebRoute::Records(repository, kind, after) => output(crate::records::list(
            store,
            credential,
            &repository,
            kind,
            &after,
            30,
        )?),
        WebRoute::Record(repository, root) => output(crate::records::detail(
            store,
            credential,
            &repository,
            root,
            options.task_limits,
        )?),
        WebRoute::Review(key) => output(crate::web_review::inspect(
            store, credential, options, &key,
        )?),
        WebRoute::ReviewContent(key, digest) => {
            let (bytes, file) = store.open_review_content(credential, &key, &digest)?;
            Ok(Output::File {
                bytes,
                digest,
                file,
            })
        }
        WebRoute::Approve(key) => execute_native(
            store,
            credential,
            options,
            Route::Request(key, RequestAction::Approve),
            &mut &raw[..],
        ),
        WebRoute::Users { after } => {
            let mut items = store.local_users(credential, &after, 51)?;
            let more = items.len() > 50;
            items.truncate(50);
            let next = if more {
                items.last().map(|v| v.actor_id.clone())
            } else {
                None
            };
            output(alt_server_store::Page { items, next })
        }
        WebRoute::CreateUser => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                login_name: String,
                display_name: String,
                password: String,
                administrator: bool,
            }
            let input: Input = serde_json::from_slice(raw)?;
            let actor = format!("user-{}", alt_browser_auth::secret());
            output(store.create_local_user(
                credential,
                &actor,
                &input.login_name,
                &input.display_name,
                &input.password,
                input.administrator,
            )?)
        }
        WebRoute::UserState(actor) => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                disabled: bool,
            }
            let input: Input = serde_json::from_slice(raw)?;
            store.disable_local_user(credential, &actor, input.disabled)?;
            output(json!({"actor_id":actor,"disabled":input.disabled}))
        }
        _ => Err(Error::Precondition("browser route dispatch")),
    }
}
