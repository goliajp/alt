//! What is queued for a channel beside a mapping: a send taken up again, a withdrawal, the repair of a binding.

use super::*;

impl Store {
    /// Explicitly resume this actor's pending send using a fresh credential.
    /// The signed statement, request identity and original actor stay unchanged.
    /// A different actor cannot take over an old statement by guessing its ID.
    pub fn reauthorize_change_event(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        channel: &str,
        config: &str,
        digest: &str,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (_, auth, _) = binding(
            &mut tx, &self.spec, scope, credential, channel, config, true,
        )?;
        let row=tx.query_opt("SELECT actor_id,author_credential,delivered_at,configuration_digest FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND event_digest=$4", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&digest])?.ok_or(Error::Precondition("change-link outgoing event missing"))?;
        if row.get::<_, String>(0) != auth.actor_id || row.get::<_, String>(3) != config {
            return Err(Error::Unauthorized);
        }
        if row.get::<_, Option<i64>>(2).is_none() && row.get::<_, String>(1) != credential.digest {
            tx.execute("UPDATE alt_link_outbox SET author_credential=$5 WHERE repository=$1 AND epoch=$2 AND channel=$3 AND event_digest=$4", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&digest,&credential.digest])?;
            crate::browser::audit_details(
                &mut tx,
                &auth.actor_id,
                "change_link.reauthorized",
                &scope.repository_instance_id,
                &Jsonb(alt_json::json!({"event_digest":digest,"channel":channel})),
            )?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    /// Original signed fact under current repository read permission. A
    /// migrated or disabled pairing remains history, never live authorization.
    pub(crate) fn change_event_once(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
    ) -> Result<link::HistoricalEvent> {
        crate::bytes::check_digest(digest)?;
        let mut tx = crate::reading::transaction(&mut self.client, &self.spec)?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = crate::reading::snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        if current.scope != *scope {
            return Err(Error::Unauthorized);
        }
        crate::store::authorize(&mut tx, &self.spec, credential, &current)?;
        let event = link_history::get(&mut tx, &scope.repository_instance_id, digest)?;
        tx.commit()?;
        Ok(event)
    }
}
impl Store {
    /// A withdrawal names the exact currently accepted mapping. Its own queue
    /// predecessor can be a more recent, durably refused correction.
    pub fn enqueue_change_withdrawal(
        &mut self,
        credential: &Credential,
        request: &ChangeWithdrawalRequest,
    ) -> Result<link::HistoricalEvent> {
        request.key.validate()?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (_, auth, channel) = binding(
            &mut tx,
            &self.spec,
            &request.key.scope,
            credential,
            &request.channel,
            &request.configuration_digest,
            true,
        )?;
        if auth.actor_id != request.key.actor_id {
            return Err(Error::Unauthorized);
        }
        let intent = alt_hash::blake3::hash(&alt_json::to_vec(request)?)
            .to_hex()
            .to_string();
        if let Some(value) = queued_request(&mut tx, &request.key, &intent)? {
            tx.commit()?;
            return Ok(value);
        }
        let mapping = accepted_mapping(
            &mut tx,
            &request.key.scope,
            &channel.channel,
            &request.mapping_event_digest,
        )?
        .ok_or(Error::Precondition("change-link accepted mapping changed"))?;
        if mapping.release.application != channel.application {
            return Err(Error::Unauthorized);
        }
        let history = append_outbox(
            &mut tx,
            &self.spec,
            &self.signing_key,
            Outgoing {
                key: &request.key,
                credential,
                channel: &channel,
                intent: &intent,
                entity: mapping_entity(&mapping.release)?,
                expected_previous: Some(&request.expected_previous),
                body: link::Body::Withdraw {
                    previous_mapping_digest: request.mapping_event_digest.clone(),
                    reason: request.reason.clone(),
                    declared_by: auth.actor_id,
                },
            },
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(history)
    }
}
impl Store {
    /// Queue notification of an already committed native claim. This separate
    /// transaction can wait for quota or peer recovery without undoing Task
    /// publication. The caller cannot supply a Task or publication identity.
    pub fn enqueue_repair_binding(
        &mut self,
        credential: &Credential,
        request: &RepairBindingRequest,
    ) -> Result<link::HistoricalEvent> {
        request.key.validate()?;
        crate::bytes::check_digest(&request.repair_request_digest)?;
        let scope = &request.key.scope;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (current, auth, channel) = binding(
            &mut tx,
            &self.spec,
            scope,
            credential,
            &request.channel,
            &request.configuration_digest,
            true,
        )?;
        if auth.actor_id != request.key.actor_id {
            return Err(Error::Unauthorized);
        }
        let row = tx.query_opt("SELECT task_id,publication_digest FROM alt_link_task_claims WHERE repository=$1 AND epoch=$2 AND channel=$3 AND repair_digest=$4", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel.channel,&request.repair_request_digest])?.ok_or(Error::Precondition("repair task not published"))?;
        let original = link_history::get(
            &mut tx,
            &scope.repository_instance_id,
            &request.repair_request_digest,
        )?;
        let link::Body::RepairRequest(repair) = &original.statement.event.body else {
            return Err(Error::Integrity("repair claim source"));
        };
        if !current.refs.contains_key(&repair.target_ref)
            || !auth
                .grants
                .iter()
                .any(|g| g.update && g.refs.matches(&repair.target_ref))
        {
            return Err(Error::Unauthorized);
        }
        let intent = alt_hash::blake3::hash(&alt_json::to_vec(request)?)
            .to_hex()
            .to_string();
        let history = append_outbox(
            &mut tx,
            &self.spec,
            &self.signing_key,
            Outgoing {
                key: &request.key,
                credential,
                channel: &channel,
                intent: &intent,
                entity: format!("task-{}", request.repair_request_digest),
                expected_previous: None,
                body: link::Body::TaskBound {
                    repair_request_digest: request.repair_request_digest.clone(),
                    task: row.get(0),
                    publication_digest: row.get(1),
                },
            },
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(history)
    }
}
