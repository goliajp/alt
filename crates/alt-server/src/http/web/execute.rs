//! A browser request once its body is received: session authority, then the store.
use super::input::{DeviceDecision, Empty, Login, NewUser, UserState};
use super::*;
use alt_json::{Value, json};
use alt_server_store::{Credential, Store};
use std::io::Read;

/// what an admitted browser request runs with, on a worker thread
pub(super) struct Call {
    pub(super) factory: pool::Pool,
    pub(super) options: NativeOptions,
    pub(super) browser: WebOptions,
    pub(super) token: Option<String>,
    pub(super) peer: IpAddr,
}
impl Call {
    /// the output and, when the session changes, the cookie to set
    pub(super) fn run(
        self,
        route: WebRoute,
        source: alt_os::temp::NamedTempFile,
    ) -> Result<(Output, Option<String>)> {
        let Self {
            factory,
            options,
            browser,
            token,
            peer,
        } = self;
        let login = route == WebRoute::Login;
        let mut source = source;
        source.as_file_mut().rewind()?;
        let mut store = factory.open()?;
        store.check_browser_binding(Some(&browser.origin))?;
        if let WebRoute::Portable(selected) = &route {
            let token = token.as_deref().ok_or(Error::Unauthorized)?;
            let credential = factory.authenticate(&mut store, token)?;
            store.browser_session(&credential, &browser.origin)?;
            let output = crate::portable::execute(
                &mut store,
                &credential,
                &options,
                selected.clone(),
                source.as_file_mut(),
                &mut |store| {
                    let current = factory.authenticate(store, token)?;
                    store.browser_session(&current, &browser.origin)?;
                    Ok(current)
                },
            )?;
            return Ok((output, None));
        }
        let mut raw = Vec::new();
        source.as_file_mut().read_to_end(&mut raw)?;
        if route == WebRoute::Config {
            return Ok((
                Output::Json(
                    json!({"deployment_mode":store.spec().deployment_mode,"workspace_id":store.spec().workspace_id}),
                ),
                None,
            ));
        }
        if login {
            let input: Login = alt_json::from_slice(&raw)?;
            let authenticated = store
                .browser_login(
                    &browser.origin,
                    &input.login_name,
                    &input.password,
                    peer,
                    browser.session_ttl_ms,
                )?
                .ok_or(Error::Unauthorized)?;
            let credential = store.authenticate(&authenticated.token)?;
            let session = session(
                &mut store,
                &credential,
                &browser.origin,
                &authenticated.token,
                &factory.signing_key,
            )?;
            let cookie = browser
                .origin
                .set_cookie(&authenticated.token, browser.session_ttl_ms / 1000)
                .map_err(|_| Error::Integrity("session cookie"))?;
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
                let output = crate::change_links::api::execute(
                    &mut store,
                    &credential,
                    &factory.change_links,
                    selected,
                    &mut &raw[..],
                )?;
                Ok((output, None))
            }
            WebRoute::Session => Ok((
                Output::Json(session(
                    &mut store,
                    &credential,
                    &browser.origin,
                    &token,
                    &factory.signing_key,
                )?),
                None,
            )),
            WebRoute::Navigation => {
                let cloud = factory.cloud.as_ref().ok_or(Error::Unauthorized)?;
                Ok((
                    crate::native::json_serde(&cloud.navigation(&mut store, &token)?)?,
                    None,
                ))
            }
            WebRoute::Sessions => Ok((
                output(store.browser_sessions(&credential, &browser.origin)?)?,
                None,
            )),
            WebRoute::DeviceConsent(code) => Ok((
                output(store.device_consent(&credential, &browser.origin, &code, peer)?)?,
                None,
            )),
            WebRoute::DeviceDecision => {
                let input: DeviceDecision = alt_json::from_slice(&raw)?;
                let consent =
                    store.device_consent(&credential, &browser.origin, &input.user_code, peer)?;
                let local = crate::http::device::credential(&factory.signing_key, &consent.id);
                let sealed = if input.allow {
                    if let Some(cloud) = &factory.cloud {
                        let descriptor = store.cloud_refresh(&token)?;
                        let grant = cloud
                            .sealer
                            .open(&descriptor.sealed_grant, &descriptor.credential_digest)
                            .map_err(|_| Error::Unauthorized)?;
                        Some(
                            cloud
                                .sealer
                                .seal(&grant, &alt_server_store::cloud_credential_digest(&local)?)
                                .map_err(|_| Error::Integrity("Cloud grant encryption"))?,
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                Ok((
                    output(store.decide_device_authorization(
                        &credential,
                        &browser.origin,
                        &input.user_code,
                        input.allow,
                        &local,
                        sealed.as_deref(),
                    )?)?,
                    None,
                ))
            }
            WebRoute::AgentDelegations(after) => {
                Ok((output(store.agent_delegations(&credential, &after)?)?, None))
            }
            WebRoute::RevokeAgent(id) => {
                let _: Empty = alt_json::from_slice(&raw)?;
                store.revoke_agent(&credential, &id)?;
                Ok((Output::Json(json!({"revoked":true})), None))
            }
            WebRoute::ClientCredentials => {
                Ok((output(store.client_credentials(&credential)?)?, None))
            }
            WebRoute::RevokeClientCredential(id) => {
                let _: Empty = alt_json::from_slice(&raw)?;
                store.revoke_client_credential(&credential, &id)?;
                Ok((Output::Json(json!({"revoked":true})), None))
            }
            WebRoute::RevokeSession(id) => {
                let _: Empty = alt_json::from_slice(&raw)?;
                store.revoke_browser_session(&credential, &browser.origin, &id)?;
                Ok((Output::Json(json!({"revoked":true})), None))
            }
            WebRoute::Logout => {
                let _: Empty = alt_json::from_slice(&raw)?;
                store.browser_logout(&credential, &browser.origin)?;
                Ok((
                    Output::Json(json!({"signed_out":true})),
                    Some(browser.origin.clear_cookie()),
                ))
            }
            route => execute(&mut store, &credential, &options, route, &raw).map(|out| (out, None)),
        }
    }
}
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
fn output(value: impl alt_json::ToJson) -> Result<Output> {
    Ok(Output::Json(alt_json::to_value(value)?))
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
            let input: NewUser = alt_json::from_slice(raw)?;
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
            let input: UserState = alt_json::from_slice(raw)?;
            store.disable_local_user(credential, &actor, input.disabled)?;
            output(json!({"actor_id":actor,"disabled":input.disabled}))
        }
        _ => Err(Error::Precondition("browser route dispatch")),
    }
}
