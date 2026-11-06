//! Bounded association requests use the same origin, credential and outcome
//! handling as native publication. Callers preserve request identities.
use super::{Error, Result, Transport, digest, id, scope_path};
use alt_remote_protocol::{RequestKey, Scope, change_links::*};
use ed25519_dalek::VerifyingKey;
use golia_change_link as link;

fn path(scope: &Scope) -> Result<String> {
    Ok(format!("{}/changes", scope_path(scope)?))
}
fn protocol<T>(value: link::Result<T>) -> Result<T> {
    value.map_err(|_| Error::Invalid("change-link signature or identity"))
}
fn local(endpoint: &link::Endpoint, scope: &Scope) -> bool {
    endpoint.product == link::Product::Alt
        && endpoint.installation == scope.installation_id
        && endpoint.scope == scope.repository_instance_id
        && endpoint.generation.0 == scope.recovery_epoch
}
fn history(value: &link::HistoricalEvent, expected: &str) -> Result<()> {
    protocol(value.validate())?;
    if protocol(value.statement.digest())? != expected {
        return Err(Error::Invalid("change-link history identity"));
    }
    Ok(())
}
fn outgoing(
    value: &link::HistoricalEvent,
    scope: &Scope,
    channel: &str,
    pinned: &[u8; 32],
) -> Result<()> {
    protocol(value.validate())?;
    let event = &value.statement.event;
    let key =
        VerifyingKey::from_bytes(pinned).map_err(|_| Error::Invalid("change-link receipt key"))?;
    if !local(&event.issuer, scope) || event.channel != channel {
        return Err(Error::Invalid("change-link outgoing scope"));
    }
    protocol(
        value
            .statement
            .verify(&key, channel, &event.issuer, &event.receiver),
    )
}
impl Transport {
    pub fn change_channels(&self, scope: &Scope) -> Result<Vec<ChangeChannelStatus>> {
        let values: Vec<ChangeChannelStatus> =
            self.json("GET", &format!("{}/channels", path(scope)?), b"", None)?;
        if values.len() > 100 {
            return Err(Error::Limit("change-link channels"));
        }
        for value in &values {
            id(&value.channel)?;
            id(&value.application)?;
            digest(&value.configuration_digest)?;
            protocol(value.peer.validate())?;
            if value.peer.product != link::Product::Sentori {
                return Err(Error::Invalid("change-link peer product"));
            }
        }
        Ok(values)
    }
    pub fn change_events(&self, scope: &Scope, after: Option<&str>) -> Result<ChangeEventPage> {
        let mut url = format!("{}/events", path(scope)?);
        if let Some(after) = after {
            digest(after)?;
            url.push_str(&format!("?after={after}"));
        }
        let value: ChangeEventPage = self.json("GET", &url, b"", None)?;
        let mut previous = after;
        if value.entries.len() > 50 {
            return Err(Error::Limit("change-link page"));
        }
        for entry in &value.entries {
            digest(&entry.digest)?;
            id(&entry.channel)?;
            id(&entry.entity)?;
            if previous.is_some_and(|p| p >= entry.digest.as_str()) {
                return Err(Error::Invalid("change-link page order"));
            }
            previous = Some(&entry.digest);
        }
        if let Some(next) = &value.next
            && (value.entries.len() != 50 || previous != Some(next.as_str()))
        {
            return Err(Error::Invalid("change-link page cursor"));
        }
        Ok(value)
    }
    pub fn change_event(&self, scope: &Scope, event: &str) -> Result<link::HistoricalEvent> {
        digest(event)?;
        let value = self.json(
            "GET",
            &format!("{}/events/{event}", path(scope)?),
            b"",
            None,
        )?;
        history(&value, event)?;
        Ok(value)
    }
    pub fn change_request(&self, key: &RequestKey) -> Result<Option<link::HistoricalEvent>> {
        key.validate()?;
        let value: Option<link::HistoricalEvent> = self.json(
            "GET",
            &format!(
                "{}/requests/{}/{}",
                path(&key.scope)?,
                key.actor_id,
                key.request_id
            ),
            b"",
            None,
        )?;
        if let Some(value) = &value {
            protocol(value.validate())?;
        }
        Ok(value)
    }
    pub fn repair_source(
        &self,
        scope: &Scope,
        event: &str,
        pinned: &[u8; 32],
    ) -> Result<link::RepairSource> {
        digest(event)?;
        let value: link::RepairSource = self.json(
            "GET",
            &format!("{}/repairs/{event}/source", path(scope)?),
            b"",
            None,
        )?;
        protocol(value.validate())?;
        let key = VerifyingKey::from_bytes(pinned)
            .map_err(|_| Error::Invalid("change-link receipt key"))?;
        protocol(value.acceptance.verify(&value.request, &key))?;
        if protocol(value.request.digest())? != event
            || !local(&value.request.event.receiver, scope)
        {
            return Err(Error::Invalid("change-link repair source scope"));
        }
        Ok(value)
    }
    pub fn repair_claim(&self, scope: &Scope, event: &str) -> Result<Option<RepairTaskClaim>> {
        digest(event)?;
        let value: Option<RepairTaskClaim> = self.json(
            "GET",
            &format!("{}/repairs/{event}/task", path(scope)?),
            b"",
            None,
        )?;
        if let Some(value) = &value {
            digest(&value.task)?;
            digest(&value.publication_digest)?;
            if value.repair_request_digest != event {
                return Err(Error::Invalid("change-link repair claim identity"));
            }
        }
        Ok(value)
    }
    pub fn change_mapping(
        &self,
        request: &ChangeMappingRequest,
        pinned: &[u8; 32],
    ) -> Result<link::HistoricalEvent> {
        request.key.validate()?;
        id(&request.channel)?;
        digest(&request.configuration_digest)?;
        digest(&request.delivery_id)?;
        let value: link::HistoricalEvent = self.json(
            "POST",
            &format!("{}/mappings", path(&request.key.scope)?),
            &self.encode(request)?,
            Some(&request.key),
        )?;
        outgoing(&value, &request.key.scope, &request.channel, pinned)?;
        if !matches!(&value.statement.event.body, link::Body::Mapping(m) if m.delivery.delivery == request.delivery_id && m.release == request.release && m.declared_by == request.key.actor_id)
            || value.statement.event.previous_digest != request.expected_previous
        {
            return Err(Error::Invalid("change-link mapping response"));
        }
        Ok(value)
    }
    pub fn change_withdrawal(
        &self,
        request: &ChangeWithdrawalRequest,
        pinned: &[u8; 32],
    ) -> Result<link::HistoricalEvent> {
        request.key.validate()?;
        id(&request.channel)?;
        digest(&request.configuration_digest)?;
        let value: link::HistoricalEvent = self.json(
            "POST",
            &format!("{}/withdrawals", path(&request.key.scope)?),
            &self.encode(request)?,
            Some(&request.key),
        )?;
        outgoing(&value, &request.key.scope, &request.channel, pinned)?;
        if !matches!(&value.statement.event.body, link::Body::Withdraw { previous_mapping_digest,reason,declared_by } if previous_mapping_digest == &request.mapping_event_digest && reason == &request.reason && declared_by == &request.key.actor_id)
            || value.statement.event.previous_digest.as_deref()
                != Some(request.expected_previous.as_str())
        {
            return Err(Error::Invalid("change-link withdrawal response"));
        }
        Ok(value)
    }
    pub fn change_reauthorize(
        &self,
        scope: &Scope,
        channel: &str,
        event: &str,
    ) -> Result<link::HistoricalEvent> {
        id(channel)?;
        digest(event)?;
        // Repeating this permission refresh cannot enqueue another statement.
        let value = self.json(
            "POST",
            &format!(
                "{}/channels/{channel}/events/{event}/reauthorize",
                path(scope)?
            ),
            b"",
            None,
        )?;
        history(&value, event)?;
        Ok(value)
    }
}
