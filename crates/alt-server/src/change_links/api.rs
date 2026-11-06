use super::{
    ChannelRuntime,
    route::{Action, ChangeRoute},
};
use crate::{
    Output,
    native::{body, empty, json},
};
use alt_remote_protocol::{RequestKey, Scope};
use alt_server_store::{Credential, Error, Result, Store};
use std::io::Read;

fn configured<'a>(
    channels: &'a [ChannelRuntime],
    scope: &Scope,
    channel: &str,
    digest: Option<&str>,
) -> Result<&'a ChannelRuntime> {
    channels
        .iter()
        .find(|c| c.matches(scope, channel) && digest.is_none_or(|d| d == c.digest))
        .ok_or(Error::Unauthorized)
}
fn bound_key(scope: &Scope, key: &RequestKey) -> Result<()> {
    if scope != &key.scope {
        return Err(Error::Unauthorized);
    }
    key.validate()?;
    Ok(())
}
pub(crate) fn request_bytes(
    store: &mut Store,
    credential: &Credential,
    route: &ChangeRoute,
) -> Result<u64> {
    match route {
        ChangeRoute::Inbox => {
            store.principal(credential)?;
        }
        ChangeRoute::Scoped(scope, _) => {
            if store
                .repository_info(credential, &scope.repository_instance_id)?
                .scope
                != *scope
            {
                return Err(Error::Unauthorized);
            }
        }
    }
    Ok(if route.has_json_body() {
        golia_change_link::MAX_BYTES as u64
    } else {
        0
    })
}
pub(crate) fn execute(
    store: &mut Store,
    credential: &Credential,
    channels: &[ChannelRuntime],
    route: ChangeRoute,
    source: &mut dyn Read,
) -> Result<Output> {
    if !route.has_json_body() {
        empty(source)?;
    }
    match route {
        ChangeRoute::Inbox => {
            let event: golia_change_link::SignedEvent = body(source, golia_change_link::MAX_BYTES)?;
            let endpoint = &event.event.receiver;
            let scope = Scope {
                installation_id: endpoint.installation.clone(),
                repository_instance_id: endpoint.scope.clone(),
                recovery_epoch: endpoint.generation.0,
            };
            let channel = configured(channels, &scope, &event.event.channel, None)?;
            json(&store.receive_change_event(credential, &scope, &channel.digest, &event)?)
        }
        ChangeRoute::Scoped(scope, action) => match action {
            Action::Channels => {
                let mut values = store.change_channels(credential, &scope)?;
                for value in &mut values {
                    let runtime = configured(
                        channels,
                        &scope,
                        &value.channel,
                        Some(&value.configuration_digest),
                    );
                    value.enabled &= runtime.is_ok();
                    value.peer_browser_origin = runtime
                        .ok()
                        .and_then(|c| c.config.peer_browser_origin.clone());
                }
                json(&values)
            }
            Action::Events(after) => {
                json(&store.change_events(credential, &scope, after.as_deref())?)
            }
            Action::Event(digest) => json(&store.change_event(credential, &scope, &digest)?),
            Action::Request(actor_id, request_id) => json(&store.change_request_event(
                credential,
                &RequestKey {
                    scope,
                    actor_id,
                    request_id,
                },
            )?),
            Action::Source(digest) => {
                let proof = store.repair_task_source(credential, &scope, &digest)?;
                let runtime = configured(channels, &scope, &proof.request.event.channel, None)?;
                if !store.change_channels(credential, &scope)?.iter().any(|c| {
                    c.enabled
                        && c.channel == runtime.config.channel
                        && c.configuration_digest == runtime.digest
                }) {
                    return Err(Error::Unauthorized);
                }
                json(&proof)
            }
            Action::Claim(digest) => json(&store.repair_task_claim(credential, &scope, &digest)?),
            Action::Mapping => {
                let input: alt_server_store::ChangeMappingRequest =
                    body(source, golia_change_link::MAX_BYTES)?;
                bound_key(&scope, &input.key)?;
                configured(
                    channels,
                    &scope,
                    &input.channel,
                    Some(&input.configuration_digest),
                )?;
                json(&store.enqueue_change_mapping(credential, &input)?)
            }
            Action::Withdraw => {
                let input: alt_server_store::ChangeWithdrawalRequest =
                    body(source, golia_change_link::MAX_BYTES)?;
                bound_key(&scope, &input.key)?;
                configured(
                    channels,
                    &scope,
                    &input.channel,
                    Some(&input.configuration_digest),
                )?;
                json(&store.enqueue_change_withdrawal(credential, &input)?)
            }
            Action::Bind => {
                let input: alt_server_store::RepairBindingRequest =
                    body(source, golia_change_link::MAX_BYTES)?;
                bound_key(&scope, &input.key)?;
                configured(
                    channels,
                    &scope,
                    &input.channel,
                    Some(&input.configuration_digest),
                )?;
                json(&store.enqueue_repair_binding(credential, &input)?)
            }
            Action::Reauthorize(channel, digest) => {
                let config = configured(channels, &scope, &channel, None)?;
                store.reauthorize_change_event(
                    credential,
                    &scope,
                    &channel,
                    &config.digest,
                    &digest,
                )?;
                json(&store.change_event(credential, &scope, &digest)?)
            }
        },
    }
}
