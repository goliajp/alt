//! Transactional product events. Network I/O belongs outside these transactions.
use crate::change_channel::{authority, endpoint, installed};
use crate::link_history;
use crate::store::{instance, integer, now, snapshot, unsigned};
use crate::{ChangeChannel, Credential, Error, InstanceSpec, Result, Store};
use alt_remote_protocol::{RequestKey, Scope};
use alt_repository_service::{Authorization, RepositorySnapshot};
use golia_change_link as link;
use postgres::Transaction;

const RECEIPT_RESERVATION: u64 = 4096;
pub use alt_remote_protocol::change_links::ChangeMappingRequest;

fn protocol<T>(result: link::Result<T>) -> Result<T> {
    result.map_err(|_| Error::Precondition("change-link signature, identity or revision"))
}
fn peer_key(channel: &ChangeChannel) -> Result<ed25519_dalek::VerifyingKey> {
    ed25519_dalek::VerifyingKey::from_bytes(
        &alt_provenance::decode_hex::<32>(&channel.peer_public_key)
            .map_err(|_| Error::Integrity("change-link key"))?,
    )
    .map_err(|_| Error::Integrity("change-link key"))
}
pub(crate) fn binding(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    scope: &Scope,
    credential: &Credential,
    channel: &str,
    config: &str,
    write: bool,
) -> Result<(RepositorySnapshot, Authorization, ChangeChannel)> {
    let current = snapshot(tx, spec, &scope.repository_instance_id)?;
    if current.scope != *scope {
        return Err(Error::Unauthorized);
    }
    let auth = authority(tx, spec, credential, &current, write)?;
    let channel = installed(tx, scope, channel)?;
    if channel.configuration_digest != config {
        return Err(Error::Unauthorized);
    }
    authority(
        tx,
        spec,
        &Credential {
            digest: channel.local_credential_digest.clone(),
        },
        &current,
        write,
    )?;
    Ok((current, auth, channel))
}
pub(crate) fn integration(credential: &Credential, channel: &ChangeChannel) -> Result<()> {
    if credential.digest != channel.local_credential_digest {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
fn mapping_entity(release: &link::Release) -> Result<String> {
    Ok(format!(
        "mapping-{}",
        blake3::hash(&serde_json::to_vec(&(
            &release.application,
            &release.release,
            &release.environment
        ))?)
        .to_hex()
    ))
}
fn current_set(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    channel: &str,
    entity: &str,
    kind: &str,
    digest: &str,
) -> Result<()> {
    tx.execute("INSERT INTO alt_link_current(repository,epoch,channel,entity,kind,event_digest) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(repository,epoch,channel,entity) DO UPDATE SET kind=excluded.kind,event_digest=excluded.event_digest", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&entity,&kind,&digest])?;
    Ok(())
}
fn accepted_mapping(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    channel: &str,
    digest: &str,
) -> Result<Option<link::Mapping>> {
    let row=tx.query_opt("SELECT event_digest FROM alt_link_current WHERE repository=$1 AND epoch=$2 AND channel=$3 AND kind='mapping' AND event_digest=$4", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&digest])?;
    if row.is_none() {
        // The receiver may already have applied this mapping while our sender
        // is still reconciling its lost response. Its dependent facts must
        // remain retryable until we save that original receipt.
        let pending: bool = tx.query_one(
            "SELECT EXISTS(SELECT 1 FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND event_digest=$4 AND delivered_at IS NULL)",
            &[&scope.repository_instance_id, &integer(scope.recovery_epoch)?, &channel, &digest],
        )?.get(0);
        if pending
            && matches!(
                link_history::get(tx, &scope.repository_instance_id, digest)?
                    .statement
                    .event
                    .body,
                link::Body::Mapping(_)
            )
        {
            return Err(Error::Precondition("change-link mapping receipt pending"));
        }
        return Ok(None);
    }
    let history = link_history::get(tx, &scope.repository_instance_id, digest)?;
    match history.statement.event.body {
        link::Body::Mapping(mapping) => Ok(Some(mapping)),
        _ => Err(Error::Integrity("change-link mapping projection")),
    }
}

struct Outgoing<'a> {
    key: &'a RequestKey,
    credential: &'a Credential,
    channel: &'a ChangeChannel,
    intent: &'a str,
    entity: String,
    expected_previous: Option<&'a str>,
    body: link::Body,
}
fn queued_request(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
    intent: &str,
) -> Result<Option<link::HistoricalEvent>> {
    let scope = &key.scope;
    if let Some(row)=tx.query_opt("SELECT intent_digest,event_digest FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&key.actor_id,&key.request_id])? {
        if row.get::<_,String>(0)!=intent {return Err(Error::Precondition("change-link request changed"))}
        return Ok(Some(link_history::get(tx,&scope.repository_instance_id,&row.get::<_,String>(1))?))
    }
    Ok(None)
}
/// Caller holds the repository lock and validated current actor+integration.
fn append_outbox(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    signing: &ed25519_dalek::SigningKey,
    input: Outgoing<'_>,
) -> Result<link::HistoricalEvent> {
    let Outgoing {
        key,
        credential,
        channel,
        intent,
        entity,
        expected_previous,
        body,
    } = input;
    if let Some(value) = queued_request(tx, key, intent)? {
        return Ok(value);
    }
    let scope = &key.scope;
    let head=tx.query_opt("SELECT revision,event_digest FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND entity=$4 ORDER BY revision DESC LIMIT 1", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel.channel,&entity])?;
    let previous = head.as_ref().map(|r| r.get::<_, String>(1));
    if previous.as_deref() != expected_previous {
        return Err(Error::Precondition("change-link mapping head changed"));
    }
    let revision = match head {
        Some(row) => unsigned(row.get(0))?
            .checked_add(1)
            .ok_or(Error::Limit("change-link revision"))?,
        None => 1,
    };
    let time = now(tx)?;
    let signed = protocol(link::SignedEvent::sign(
        link::Event {
            version: link::VERSION,
            channel: channel.channel.clone(),
            issuer: endpoint(spec, scope),
            receiver: channel.peer.clone(),
            entity: entity.clone(),
            revision: link::Counter(revision),
            previous_digest: previous,
            issued_unix_ms: link::Counter(time),
            body,
        },
        signing,
    ))?;
    let digest = protocol(signed.digest())?;
    let history = link::HistoricalEvent {
        statement: signed,
        source_public_key: alt_provenance::encode_hex(signing.verifying_key().as_bytes()),
        acknowledgement: None,
    };
    link_history::retain(
        tx,
        spec,
        &scope.repository_instance_id,
        &history,
        0,
        RECEIPT_RESERVATION,
    )?;
    tx.execute("INSERT INTO alt_link_outbox(repository,epoch,channel,configuration_digest,entity,revision,event_digest,actor_id,author_credential,request_id,intent_digest,receipt_reservation,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel.channel,&channel.configuration_digest,&entity,&integer(revision)?,&digest,&key.actor_id,&credential.digest,&key.request_id,&intent,&integer(RECEIPT_RESERVATION)?,&integer(time)?])?;
    crate::browser::audit_details(
        tx,
        &key.actor_id,
        "change_link.queued",
        &scope.repository_instance_id,
        &serde_json::json!({"event_digest":digest,"channel":channel.channel}),
    )?;
    Ok(history)
}

impl Store {
    /// Project actual committed records first, then recheck current authority
    /// under the repository lock. No caller supplied commit/receipt is signed.
    pub fn enqueue_change_mapping(
        &mut self,
        credential: &Credential,
        request: &ChangeMappingRequest,
    ) -> Result<link::HistoricalEvent> {
        request.key.validate()?;
        let delivery =
            self.change_link_delivery(credential, &request.key.scope, &request.delivery_id)?;
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
        if auth.actor_id != request.key.actor_id
            || channel.application != request.release.application
        {
            return Err(Error::Unauthorized);
        }
        let intent = blake3::hash(&serde_json::to_vec(request)?)
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
                entity: mapping_entity(&request.release)?,
                expected_previous: request.expected_previous.as_deref(),
                body: link::Body::Mapping(link::Mapping {
                    delivery,
                    release: request.release.clone(),
                    declared_by: auth.actor_id,
                }),
            },
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(history)
    }

    /// A bounded discovery list; each item still requires dispatch authorization.
    pub fn pending_change_events(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        channel: &str,
        config: &str,
    ) -> Result<Vec<String>> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (_, _, installed) = binding(
            &mut tx, &self.spec, scope, credential, channel, config, false,
        )?;
        integration(credential, &installed)?;
        let rows=tx.query("SELECT event_digest FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND delivered_at IS NULL ORDER BY created_at,entity,revision LIMIT 100", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel])?;
        let result = rows.into_iter().map(|r| r.get(0)).collect();
        tx.commit()?;
        Ok(result)
    }

    /// Check the installed integration and the original enqueue actor now.
    /// Cloud adapters must refresh those current account leases before calling.
    pub fn change_dispatch(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        channel: &str,
        config: &str,
        digest: &str,
    ) -> Result<link::HistoricalEvent> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (current, _, installed) = binding(
            &mut tx, &self.spec, scope, credential, channel, config, true,
        )?;
        integration(credential, &installed)?;
        let row=tx.query_opt("SELECT author_credential,entity,revision,configuration_digest FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND event_digest=$4", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&digest])?.ok_or(Error::Precondition("change-link outgoing event missing"))?;
        if row.get::<_, String>(3) != config {
            return Err(Error::Unauthorized);
        }
        authority(
            &mut tx,
            &self.spec,
            &Credential { digest: row.get(0) },
            &current,
            true,
        )?;
        let earlier=tx.query_one("SELECT EXISTS(SELECT 1 FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND entity=$4 AND revision<$5 AND delivered_at IS NULL)", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&row.get::<_,String>(1),&row.get::<_,i64>(2)])?.get::<_,bool>(0);
        if earlier {
            return Err(Error::Precondition("change-link predecessor pending"));
        }
        let history = link_history::get(&mut tx, &scope.repository_instance_id, digest)?;
        tx.commit()?;
        Ok(history)
    }

    /// A lost response is reconciled with this original signed receipt; a later
    /// delivered revision is never replaced by a replay of an older result.
    pub fn acknowledge_change_event(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        channel: &str,
        config: &str,
        receipt: &link::Receipt,
    ) -> Result<link::HistoricalEvent> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (current, _, installed) = binding(
            &mut tx, &self.spec, scope, credential, channel, config, true,
        )?;
        integration(credential, &installed)?;
        let digest = &receipt.body.event_digest;
        let row=tx.query_opt("SELECT author_credential,receipt_reservation,delivered_at,configuration_digest FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND event_digest=$4", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&digest])?.ok_or(Error::Precondition("change-link outgoing event missing"))?;
        if row.get::<_, String>(3) != config {
            return Err(Error::Unauthorized);
        }
        authority(
            &mut tx,
            &self.spec,
            &Credential { digest: row.get(0) },
            &current,
            true,
        )?;
        let mut history = link_history::get(&mut tx, &scope.repository_instance_id, digest)?;
        let decision = protocol(receipt.verify(&history.statement, &peer_key(&installed)?))?;
        let ack = link::HistoricalAcknowledgement {
            receipt: receipt.clone(),
            receiver_public_key: installed.peer_public_key.clone(),
        };
        if row.get::<_, Option<i64>>(2).is_some() {
            if history.acknowledgement.as_ref() != Some(&ack) {
                return Err(Error::Precondition("change-link receipt changed"));
            }
            tx.commit()?;
            return Ok(history);
        }
        let event = &history.statement.event;
        let prior:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM alt_link_outbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND entity=$4 AND revision<$5 AND delivered_at IS NULL)", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&event.entity,&integer(event.revision.0)?])?.get(0);
        if prior {
            return Err(Error::Precondition("change-link predecessor pending"));
        }
        history.acknowledgement = Some(ack);
        let reserve = unsigned(row.get(1))?;
        let delta = serde_json::to_vec(&history)?.len().saturating_sub(
            serde_json::to_vec(&link_history::get(
                &mut tx,
                &scope.repository_instance_id,
                digest,
            )?)?
            .len(),
        ) as u64;
        if delta > reserve {
            return Err(Error::Limit("change-link receipt reservation"));
        }
        link_history::retain(
            &mut tx,
            &self.spec,
            &scope.repository_instance_id,
            &history,
            reserve,
            0,
        )?;
        let time = integer(now(&mut tx)?)?;
        tx.execute("UPDATE alt_link_outbox SET delivered_at=$5,receipt_reservation=0 WHERE repository=$1 AND epoch=$2 AND channel=$3 AND event_digest=$4", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&digest,&time])?;
        if decision == link::Decision::Applied {
            let kind = match history.statement.event.body {
                link::Body::Mapping(_) => "mapping",
                link::Body::Withdraw { .. } => "withdraw",
                link::Body::TaskBound { .. } => "task_bound",
                _ => return Err(Error::Integrity("change-link outgoing kind")),
            };
            current_set(
                &mut tx,
                scope,
                channel,
                &history.statement.event.entity,
                kind,
                digest,
            )?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(history)
    }

    /// Authenticate current local integration authority independently from the
    /// registered peer signature, then atomically store business result+receipt.
    pub fn receive_change_event(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        config: &str,
        signed: &link::SignedEvent,
    ) -> Result<link::Receipt> {
        let event = &signed.event;
        let digest = protocol(signed.digest())?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (current, auth, channel) = binding(
            &mut tx,
            &self.spec,
            scope,
            credential,
            &event.channel,
            config,
            true,
        )?;
        integration(credential, &channel)?;
        protocol(signed.verify(
            &peer_key(&channel)?,
            &channel.channel,
            &channel.peer,
            &endpoint(&self.spec, scope),
        ))?;
        let known=tx.query_opt("SELECT event_digest FROM alt_link_inbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND entity=$4 AND revision=$5", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel.channel,&event.entity,&integer(event.revision.0)?])?.map(|r|r.get::<_,String>(0));
        let head=tx.query_opt("SELECT revision,event_digest FROM alt_link_inbox WHERE repository=$1 AND epoch=$2 AND channel=$3 AND entity=$4 ORDER BY revision DESC LIMIT 1", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel.channel,&event.entity])?.map(|r| Ok::<_,Error>(link::Head{revision:link::Counter(unsigned(r.get(0))?),digest:r.get(1)})).transpose()?;
        if protocol(link::classify(signed, head.as_ref(), known.as_deref()))?
            != link::Application::Apply
        {
            let history = link_history::get(&mut tx, &scope.repository_instance_id, &digest)?;
            let receipt = history
                .acknowledgement
                .ok_or(Error::Integrity("change-link inbox receipt missing"))?
                .receipt;
            tx.commit()?;
            return Ok(receipt);
        }
        let (kind, decision) = incoming_decision(&mut tx, scope, &channel, &current, &auth, event)?;
        let time = now(&mut tx)?;
        let receipt = protocol(link::Receipt::decide(
            signed,
            link::Counter(time),
            decision,
            &self.signing_key,
        ))?;
        let history = link::HistoricalEvent {
            statement: signed.clone(),
            source_public_key: channel.peer_public_key.clone(),
            acknowledgement: Some(link::HistoricalAcknowledgement {
                receipt: receipt.clone(),
                receiver_public_key: alt_provenance::encode_hex(
                    self.signing_key.verifying_key().as_bytes(),
                ),
            }),
        };
        link_history::retain(
            &mut tx,
            &self.spec,
            &scope.repository_instance_id,
            &history,
            0,
            0,
        )?;
        tx.execute("INSERT INTO alt_link_inbox(repository,epoch,channel,configuration_digest,entity,revision,event_digest,received_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel.channel,&channel.configuration_digest,&event.entity,&integer(event.revision.0)?,&digest,&integer(time)?])?;
        if decision == link::Decision::Applied {
            current_set(
                &mut tx,
                scope,
                &channel.channel,
                &event.entity,
                kind,
                &digest,
            )?;
        }
        crate::browser::audit_details(
            &mut tx,
            &auth.actor_id,
            "change_link.received",
            &scope.repository_instance_id,
            &serde_json::json!({"event_digest":digest,"decision":decision}),
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(receipt)
    }
}

fn incoming_decision(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    channel: &ChangeChannel,
    current: &RepositorySnapshot,
    auth: &Authorization,
    event: &link::Event,
) -> Result<(&'static str, link::Decision)> {
    use link::{Body, Decision, Refusal};
    match &event.body {
        Body::RepairRequest(request) => {
            if !event.entity.starts_with("repair-") {
                return Err(Error::Precondition("change-link repair entity"));
            }
            if request
                .observed_version
                .application
                .as_ref()
                .is_some_and(|v| v != &channel.application)
                || !current.refs.contains_key(&request.target_ref)
                || !auth
                    .grants
                    .iter()
                    .any(|g| g.update && g.refs.matches(&request.target_ref))
            {
                return Ok((
                    "repair_request",
                    Decision::Refused(Refusal::TargetUnavailable),
                ));
            }
            if let Some(digest) = &request.mapping_event_digest {
                let mapping = accepted_mapping(tx, scope, &channel.channel, digest)?;
                let matches = mapping.is_some_and(|m| {
                    request
                        .observed_version
                        .application
                        .as_ref()
                        .is_none_or(|v| v == &m.release.application)
                        && request
                            .observed_version
                            .release
                            .as_ref()
                            .is_none_or(|v| v == &m.release.release)
                        && request
                            .observed_version
                            .release_name
                            .as_ref()
                            .is_none_or(|v| v == &m.release.release_name)
                        && request
                            .observed_version
                            .environment
                            .as_ref()
                            .is_none_or(|v| v == &m.release.environment)
                });
                if !matches {
                    return Ok(("repair_request", Decision::Refused(Refusal::StateConflict)));
                }
            }
            Ok(("repair_request", Decision::Applied))
        }
        Body::Observation(observation) => {
            if event.entity != format!("observation-{}", observation.mapping_event_digest) {
                return Err(Error::Precondition("change-link observation entity"));
            }
            let mapping = accepted_mapping(
                tx,
                scope,
                &channel.channel,
                &observation.mapping_event_digest,
            )?;
            let decision = if mapping.is_some_and(|m| {
                m.release == observation.release && m.release.application == channel.application
            }) {
                Decision::Applied
            } else {
                Decision::Refused(Refusal::StateConflict)
            };
            Ok(("observation", decision))
        }
        _ => Err(Error::Precondition("change-link incoming direction")),
    }
}

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
                &serde_json::json!({"event_digest":digest,"channel":channel}),
            )?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    /// Original signed fact under current repository read permission. A
    /// migrated or disabled pairing remains history, never live authorization.
    pub fn change_event(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
    ) -> Result<link::HistoricalEvent> {
        crate::bytes::check_digest(digest)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        if current.scope != *scope {
            return Err(Error::Unauthorized);
        }
        crate::store::authorize(&mut tx, &self.spec, credential, &current)?;
        let event = link_history::get(&mut tx, &scope.repository_instance_id, digest)?;
        tx.commit()?;
        Ok(event)
    }
}

pub use alt_remote_protocol::change_links::ChangeWithdrawalRequest;
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
        let intent = blake3::hash(&serde_json::to_vec(request)?)
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

pub use alt_remote_protocol::change_links::RepairBindingRequest;
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
        let intent = blake3::hash(&serde_json::to_vec(request)?)
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
