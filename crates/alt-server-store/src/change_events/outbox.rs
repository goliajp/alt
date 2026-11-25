//! The events that wait to be sent to a peer, in the order of their channel.

use super::*;

pub(super) struct Outgoing<'a> {
    pub(super) key: &'a RequestKey,
    pub(super) credential: &'a Credential,
    pub(super) channel: &'a ChangeChannel,
    pub(super) intent: &'a str,
    pub(super) entity: String,
    pub(super) expected_previous: Option<&'a str>,
    pub(super) body: link::Body,
}
pub(super) fn queued_request(
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
pub(super) fn append_outbox(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    signing: &alt_crypto::ed25519::SigningKey,
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
        &link_key(signing),
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
        &Jsonb(alt_json::json!({"event_digest":digest,"channel":channel.channel})),
    )?;
    Ok(history)
}
