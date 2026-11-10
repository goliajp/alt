//! Operator-selected cross-product transports. Secrets never enter events,
//! API responses or portable catalogs. Runtime configuration alone grants no
//! permission: every operation checks an explicitly installed database pairing.
use crate::{
    StoreFactory,
    operator::{self, Config},
};
use alt_remote_protocol::Scope;
use alt_server_store::{ChangeChannel, Error, RepairBindingRequest, Result};
use alt_server_telemetry::{Id, Metrics, Outcome, Phase, Route, Trace};
use golia_change_link as link;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc, time::Duration};

pub(crate) mod api;
pub mod route;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    pub scope: Scope,
    pub channel: String,
    pub application: String,
    pub peer: link::Endpoint,
    pub peer_public_key: String,
    pub target: link::transport::Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_browser_origin: Option<String>,
    pub local_credential_file: PathBuf,
    pub peer_credential_file: PathBuf,
}
impl ChannelConfig {
    pub fn validate(&self, instance: &alt_server_store::InstanceSpec) -> operator::Result<()> {
        self.scope
            .validate()
            .map_err(|_| operator::Error::Config("change-link scope"))?;
        self.peer
            .validate()
            .map_err(|_| operator::Error::Config("change-link peer"))?;
        self.target
            .url()
            .map_err(|_| operator::Error::Config("change-link destination"))?;
        if self.scope.installation_id != instance.installation_id
            || self.peer.product != link::Product::Sentori
            || !crate::route::id(&self.channel)
            || !crate::route::id(&self.application)
            || !self.local_credential_file.is_absolute()
            || !self.peer_credential_file.is_absolute()
        {
            return Err(operator::Error::Config("change-link configuration"));
        }
        if let Some(origin) = &self.peer_browser_origin {
            alt_browser_auth::PublicOrigin::parse(origin)
                .map_err(|_| operator::Error::Config("change-link browser origin"))?;
        }
        let key = alt_provenance::decode_hex::<32>(&self.peer_public_key)
            .map_err(|_| operator::Error::Config("change-link key"))?;
        ed25519_dalek::VerifyingKey::from_bytes(&key)
            .map_err(|_| operator::Error::Config("change-link key"))?;
        Ok(())
    }
    pub fn load(&self, config: &Config) -> operator::Result<ChannelRuntime> {
        self.validate(&config.instance)?;
        let local_token = config.read_credential(&self.local_credential_file)?;
        let peer_token = String::from_utf8(operator::read_regular(
            &self.peer_credential_file,
            4096,
            true,
        )?)
        .map_err(|_| operator::Error::Config("change-link peer credential"))?;
        let peer_token = peer_token
            .strip_suffix('\n')
            .unwrap_or(&peer_token)
            .to_owned();
        if peer_token.is_empty() || !peer_token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(operator::Error::Config("change-link peer credential"));
        }
        let identity = serde_json::to_vec(&(
            &self.scope,
            &self.channel,
            &self.application,
            &self.peer,
            &self.peer_public_key,
            &self.target,
            &self.peer_browser_origin,
            blake3::hash(local_token.as_bytes()).to_hex().to_string(),
            blake3::hash(peer_token.as_bytes()).to_hex().to_string(),
        ))
        .map_err(|_| operator::Error::Config("change-link configuration"))?;
        Ok(ChannelRuntime {
            config: self.clone(),
            digest: blake3::hash(&identity).to_hex().to_string(),
            local_token,
            peer_token,
        })
    }
}
#[derive(Clone)]
pub struct ChannelRuntime {
    pub(crate) config: ChannelConfig,
    pub(crate) digest: String,
    local_token: String,
    peer_token: String,
}
impl ChannelRuntime {
    pub fn install(&self, factory: &StoreFactory, admin_token: &str) -> Result<()> {
        let mut store = factory.open()?;
        let admin = factory.authenticate(&mut store, admin_token)?;
        let integration = factory.authenticate(&mut store, &self.local_token)?;
        store.install_change_channel(
            &admin,
            &self.config.scope,
            &ChangeChannel {
                channel: self.config.channel.clone(),
                application: self.config.application.clone(),
                peer: self.config.peer.clone(),
                peer_public_key: self.config.peer_public_key.clone(),
                local_credential_digest: integration.identifier().into(),
                configuration_digest: self.digest.clone(),
            },
        )
    }
    pub fn configuration_digest(&self) -> &str {
        &self.digest
    }
    pub fn matches(&self, scope: &Scope, channel: &str) -> bool {
        &self.config.scope == scope && self.config.channel == channel
    }
}

#[derive(Clone, Default)]
struct Cursors {
    claims: Option<String>,
    events: Option<String>,
}

async fn run_channel(
    factory: StoreFactory,
    channel: ChannelRuntime,
    after: Cursors,
    metrics: Arc<Metrics>,
) -> Result<Cursors> {
    let f = factory.clone();
    let c = channel.clone();
    let (pending, claims) = crate::http::spawn_observed(Phase::Queue, move || {
        let mut store = f.open()?;
        let integration = f.authenticate(&mut store, &c.local_token)?;
        // Notification is derived from a committed native Task; it carries no
        // fabricated execution or validation result. Failures leave the claim.
        let claims = store.unannounced_repair_tasks(
            &integration,
            &c.config.scope,
            &c.config.channel,
            &c.digest,
            after.claims.as_deref(),
        )?;
        let next_claims = if claims.len() == 16 {
            claims.last().cloned()
        } else {
            None
        };
        let actor = store.principal(&integration)?.actor_id;
        for digest in claims.into_iter().take(16) {
            let request = RepairBindingRequest {
                key: alt_remote_protocol::RequestKey {
                    scope: c.config.scope.clone(),
                    actor_id: actor.clone(),
                    request_id: format!("repair-task-{digest}"),
                },
                channel: c.config.channel.clone(),
                configuration_digest: c.digest.clone(),
                repair_request_digest: digest,
            };
            let _ = alt_server_store::telemetry::observe(Phase::Queue, || {
                store.enqueue_repair_binding(&integration, &request)
            });
        }
        let pending = store.pending_change_page(
            &integration,
            &c.config.scope,
            &c.config.channel,
            &c.digest,
            after.events.as_deref(),
        )?;
        Ok::<_, Error>((pending, next_claims))
    })
    .await
    .map_err(|_| Error::AuthorityUnavailable)??;
    let next = if pending.len() == 16 {
        pending.last().cloned()
    } else {
        None
    };
    for digest in pending {
        let trace = background_trace(&factory, &channel, metrics.clone());
        trace.event_identity(Id::new(&digest).expect("stored event digest"));
        // Each event attempt has its own identity, so a later event cannot
        // overwrite the identity of an earlier detached blocking worker.
        let _ = crate::http::trace_scope(
            trace.clone(),
            dispatch(factory.clone(), channel.clone(), digest, trace),
        )
        .await;
    }
    Ok(Cursors {
        claims,
        events: next,
    })
}

fn background_trace(
    factory: &StoreFactory,
    channel: &ChannelRuntime,
    metrics: Arc<Metrics>,
) -> Trace {
    let trace = Trace::background(
        metrics,
        Id::new(&factory.instance.installation_id).expect("validated instance identity"),
        Route::ChangeLink,
    );
    trace.identity(
        Id::new(&channel.config.scope.repository_instance_id),
        None,
        None,
    );
    trace
}

async fn dispatch(
    factory: StoreFactory,
    channel: ChannelRuntime,
    digest: String,
    trace: Trace,
) -> Result<()> {
    let f = factory.clone();
    let c = channel.clone();
    let outgoing = crate::http::spawn_observed(Phase::Authority, move || {
        let mut store = f.open()?;
        let credential = f.authenticate(&mut store, &c.local_token)?;
        store.change_dispatch(
            &credential,
            &c.config.scope,
            &c.config.channel,
            &c.digest,
            &digest,
        )
    })
    .await
    .map_err(|_| Error::AuthorityUnavailable)??;
    let phase = trace.phase(Phase::Dispatch);
    let key = ed25519_dalek::VerifyingKey::from_bytes(
        &alt_provenance::decode_hex::<32>(&channel.config.peer_public_key)
            .map_err(|_| Error::Unauthorized)?,
    )
    .map_err(|_| Error::Unauthorized)?;
    let receipt = match golia_change_link_http::send(
        &channel.config.target,
        &channel.peer_token,
        &outgoing.statement,
        &key,
    )
    .await
    {
        Ok(receipt) => {
            phase.finish(Outcome::Ok);
            receipt
        }
        Err(failure) => {
            // An HTTP rejection or invalid response can follow a remote commit.
            // Preserve uncertainty; no raw error/URL/credential enters the log.
            phase.finish(if failure.may_have_sent {
                Outcome::Unknown
            } else {
                match failure.code {
                    golia_change_link_http::Code::Configuration
                    | golia_change_link_http::Code::AddressPolicy => Outcome::Invalid,
                    _ => Outcome::Unavailable,
                }
            });
            return Err(if failure.may_have_sent {
                Error::OutcomeUnknown
            } else {
                Error::AuthorityUnavailable
            });
        }
    };
    crate::http::spawn_observed(Phase::Acknowledge, move || {
        let mut store = factory.open()?;
        let credential = factory.authenticate(&mut store, &channel.local_token)?;
        store.acknowledge_change_event(
            &credential,
            &channel.config.scope,
            &channel.config.channel,
            &channel.digest,
            &receipt,
        )
    })
    .await
    .map_err(|_| Error::AuthorityUnavailable)??;
    Ok(())
}

/// Serve owns this task and cancels it at shutdown. No DB transaction spans a
/// network request; a stopped sender retries the same signed event on restart.
pub(crate) async fn worker(factory: StoreFactory, metrics: Arc<Metrics>) {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let mut cursors = vec![Cursors::default(); factory.change_links.len()];
    loop {
        let mut jobs = tokio::task::JoinSet::new();
        for (index, channel) in factory.change_links.iter().cloned().enumerate() {
            let factory = factory.clone();
            let semaphore = semaphore.clone();
            let cursor = cursors[index].clone();
            let metrics = metrics.clone();
            jobs.spawn(async move {
                let Ok(_permit) = semaphore.acquire_owned().await else {
                    return (index, Cursors::default());
                };
                // Errors leave durable pending state; later independent events
                // still get a turn. No secrets or peer response text are logged.
                let trace = background_trace(&factory, &channel, metrics.clone());
                (
                    index,
                    crate::http::trace_scope(trace, run_channel(factory, channel, cursor, metrics))
                        .await
                        .unwrap_or_default(),
                )
            });
        }
        while let Some(result) = jobs.join_next().await {
            if let Ok((index, next)) = result {
                cursors[index] = next;
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
