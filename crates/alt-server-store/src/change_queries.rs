//! Bounded authenticated discovery. Public projections never expose installed
//! credential digests, operator destinations or secret-file configuration.
use crate::change_events::{binding, integration};
use crate::store::{authorize, instance, integer, snapshot};
use crate::{Credential, Error, Result, Store};
use alt_remote_protocol::{RequestKey, Scope};
use golia_change_link as link;

pub use alt_remote_protocol::change_links::ChangeChannelStatus;
pub use alt_remote_protocol::change_links::ChangeEventPage;
pub use alt_remote_protocol::change_links::ChangeEventSummary;
impl Store {
    pub fn change_channels(
        &mut self,
        credential: &Credential,
        scope: &Scope,
    ) -> Result<Vec<ChangeChannelStatus>> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        if current.scope != *scope {
            return Err(Error::Unauthorized);
        }
        authorize(&mut tx, &self.spec, credential, &current)?;
        let rows=tx.query("SELECT specification,enabled FROM alt_link_channels WHERE repository=$1 AND epoch=$2 ORDER BY created_at DESC,channel LIMIT 100",&[&scope.repository_instance_id,&integer(scope.recovery_epoch)?])?;
        rows.into_iter()
            .map(|row| {
                let channel: crate::ChangeChannel = serde_json::from_value(row.get(0))?;
                Ok(ChangeChannelStatus {
                    peer_browser_origin: None,
                    channel: channel.channel,
                    application: channel.application,
                    peer: channel.peer,
                    configuration_digest: channel.configuration_digest,
                    enabled: row.get(1),
                })
            })
            .collect()
    }
    pub fn change_events(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        after: Option<&str>,
    ) -> Result<ChangeEventPage> {
        if let Some(after) = after {
            crate::bytes::check_digest(after)?;
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        if current.scope != *scope {
            return Err(Error::Unauthorized);
        }
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let rows=tx.query("SELECT h.event_digest,o.delivered_at,o.actor_id FROM alt_link_history h LEFT JOIN alt_link_outbox o ON o.repository=h.repository AND o.event_digest=h.event_digest AND o.epoch=$2 WHERE h.repository=$1 AND h.event_digest>$3 ORDER BY h.event_digest LIMIT 51",&[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&after.unwrap_or("")])?;
        let more = rows.len() > 50;
        let mut entries = Vec::new();
        for row in rows.into_iter().take(50) {
            let digest: String = row.get(0);
            let history =
                crate::link_history::get(&mut tx, &scope.repository_instance_id, &digest)?;
            let event = history.statement.event;
            let kind = match event.body {
                link::Body::Mapping(_) => "mapping",
                link::Body::Withdraw { .. } => "withdraw",
                link::Body::RepairRequest(_) => "repair_request",
                link::Body::TaskBound { .. } => "task_bound",
                link::Body::Observation(_) => "observation",
            };
            let actor: Option<String> = row.get(2);
            let pending = actor.is_some() && row.get::<_, Option<i64>>(1).is_none();
            entries.push(ChangeEventSummary {
                digest,
                channel: event.channel,
                entity: event.entity,
                kind: kind.into(),
                revision: event.revision,
                issued_unix_ms: event.issued_unix_ms,
                decision: history.acknowledgement.map(|a| a.receipt.body.decision),
                pending,
                can_reauthorize: pending && actor.as_deref() == Some(&auth.actor_id),
            });
        }
        let next = if more {
            entries.last().map(|e| e.digest.clone())
        } else {
            None
        };
        Ok(ChangeEventPage { entries, next })
    }
    pub fn change_request_event(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<Option<link::HistoricalEvent>> {
        key.validate()?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &key.scope.repository_instance_id)?;
        if current.scope != key.scope {
            return Err(Error::Unauthorized);
        }
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if auth.actor_id != key.actor_id {
            return Err(Error::Unauthorized);
        }
        let row=tx.query_opt("SELECT event_digest FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4",&[&key.scope.repository_instance_id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?;
        row.map(|r| {
            crate::link_history::get(
                &mut tx,
                &key.scope.repository_instance_id,
                &r.get::<_, String>(0),
            )
        })
        .transpose()
    }
    pub fn unannounced_repair_tasks(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        channel: &str,
        config: &str,
        after: Option<&str>,
    ) -> Result<Vec<String>> {
        if let Some(after) = after {
            crate::bytes::check_digest(after)?;
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (_, _, installed) = binding(
            &mut tx, &self.spec, scope, credential, channel, config, true,
        )?;
        integration(credential, &installed)?;
        let rows=tx.query("SELECT c.repair_digest FROM alt_link_task_claims c WHERE c.repository=$1 AND c.epoch=$2 AND c.channel=$3 AND c.repair_digest>$4 AND NOT EXISTS(SELECT 1 FROM alt_link_outbox o WHERE o.repository=c.repository AND o.epoch=c.epoch AND o.channel=c.channel AND o.entity='task-'||c.repair_digest) ORDER BY c.repair_digest LIMIT 16",&[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&after.unwrap_or("")])?;
        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }
    /// Worker cursor visits blocked entries as well as sendable entries, so a
    /// revoked author's backlog cannot starve unrelated actors in this pairing.
    pub fn pending_change_page(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        channel: &str,
        config: &str,
        after: Option<&str>,
    ) -> Result<Vec<String>> {
        if let Some(after) = after {
            crate::bytes::check_digest(after)?;
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (_, _, installed) = binding(
            &mut tx, &self.spec, scope, credential, channel, config, false,
        )?;
        integration(credential, &installed)?;
        let rows=tx.query("SELECT event_digest FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND delivered_at IS NULL AND event_digest>$4 ORDER BY event_digest LIMIT 16",&[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&after.unwrap_or("")])?;
        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }
}
