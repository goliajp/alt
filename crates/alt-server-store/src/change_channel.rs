//! Persisted operational installation, independent from historical public keys.
use crate::database::Transaction;
use crate::store::{authorize, instance, integer, now, require_admin, snapshot};
use crate::{Credential, Error, InstanceSpec, Result, Store};
use alt_remote_protocol::{ActorKind, RequestKey, Scope};
use alt_repository_service::{Authorization, RepositorySnapshot};
use golia_change_link::{Counter, Endpoint, Product};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeChannel {
    pub channel: String,
    pub peer: Endpoint,
    /// One exact Sentori application within the paired project.
    pub application: String,
    pub peer_public_key: String,
    /// The explicitly installed local integration account, never a bearer.
    pub local_credential_digest: String,
    /// Binds operator transport policy and secret-file content identities.
    pub configuration_digest: String,
}
impl ChangeChannel {
    fn validate(&self, scope: &Scope) -> Result<()> {
        if self.channel.is_empty()
            || self.channel.len() > 128
            || !self
                .channel
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(Error::Precondition("change-link channel identifier"));
        }
        RequestKey {
            scope: scope.clone(),
            actor_id: "integration".into(),
            request_id: self.channel.clone(),
        }
        .validate()?;
        let mut application_scope = self.peer.clone();
        application_scope.scope = self.application.clone();
        application_scope
            .validate()
            .map_err(|_| Error::Precondition("change-link application"))?;
        self.peer
            .validate()
            .map_err(|_| Error::Precondition("change-link peer scope"))?;
        if self.peer.product != Product::Sentori {
            return Err(Error::Precondition("change-link peer product"));
        }
        let key = alt_provenance::decode_hex::<32>(&self.peer_public_key)
            .map_err(|_| Error::Precondition("change-link peer key"))?;
        alt_crypto::ed25519::VerifyingKey::from_bytes(&key)
            .map_err(|_| Error::Precondition("change-link peer key"))?;
        crate::bytes::check_digest(&self.local_credential_digest)?;
        crate::bytes::check_digest(&self.configuration_digest)?;
        Ok(())
    }
}

pub(crate) fn endpoint(spec: &InstanceSpec, scope: &Scope) -> Endpoint {
    Endpoint {
        product: Product::Alt,
        installation: spec.installation_id.clone(),
        workspace: spec.workspace_id.clone(),
        scope: scope.repository_instance_id.clone(),
        generation: Counter(scope.recovery_epoch),
    }
}

/// The caller already holds the current repository snapshot lock. An old
/// physical restore row is never selected using its historical epoch.
pub(crate) fn installed(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    channel: &str,
) -> Result<ChangeChannel> {
    let row = tx.query_opt("SELECT specification,enabled FROM alt_link_channels WHERE repository=$1 AND epoch=$2 AND channel=$3", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel])?.ok_or(Error::Unauthorized)?;
    if !row.get::<_, bool>(1) {
        return Err(Error::Unauthorized);
    }
    let value: ChangeChannel = serde_json::from_value(row.get(0))?;
    value.validate(scope)?;
    if value.channel != channel {
        return Err(Error::Integrity("change-link channel binding"));
    }
    Ok(value)
}

pub(crate) fn authority(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
    current: &RepositorySnapshot,
    write: bool,
) -> Result<Authorization> {
    let auth = authorize(tx, spec, credential, current)?;
    if auth.attribution.kind == ActorKind::Agent
        || (write && (!current.writes_enabled || !auth.publish_records))
    {
        return Err(Error::Unauthorized);
    }
    Ok(auth)
}

impl Store {
    /// Install one immutable pairing with explicit local admin/content grants.
    /// A revoked pairing cannot be resurrected by a startup/config replay;
    /// rotation requires a fresh channel ID and fresh peer registration.
    pub fn install_change_channel(
        &mut self,
        admin: &Credential,
        scope: &Scope,
        channel: &ChangeChannel,
    ) -> Result<()> {
        channel.validate(scope)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        require_admin(&mut tx, &self.spec, admin)?;
        let owner = authority(&mut tx, &self.spec, admin, &current, true)?;
        authority(
            &mut tx,
            &self.spec,
            &Credential {
                digest: channel.local_credential_digest.clone(),
            },
            &current,
            true,
        )?;
        let existing=tx.query_opt("SELECT specification,enabled FROM alt_link_channels WHERE repository=$1 AND epoch=$2 AND channel=$3", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel.channel])?;
        if let Some(row) = existing {
            if !row.get::<_, bool>(1)
                || serde_json::from_value::<ChangeChannel>(row.get(0))? != *channel
            {
                return Err(Error::Precondition(
                    "change-link pairing is immutable; install a new channel",
                ));
            }
            tx.commit()?;
            return Ok(());
        }
        let count:i64=tx.query_one("SELECT count(*) FROM alt_link_channels WHERE repository=$1 AND epoch=$2 AND enabled", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?])?.get(0);
        if count >= 32 {
            return Err(Error::Limit("active change-link channels"));
        }
        let time = integer(now(&mut tx)?)?;
        tx.execute("INSERT INTO alt_link_channels(repository,epoch,channel,specification,enabled,created_at) VALUES($1,$2,$3,$4,true,$5)", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel.channel,&serde_json::to_value(channel)?,&time])?;
        crate::browser::audit_details(
            &mut tx,
            &owner.actor_id,
            "change_link.installed",
            &scope.repository_instance_id,
            &serde_json::json!({"channel":channel.channel,"configuration_digest":channel.configuration_digest}),
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    pub fn disable_change_channel(
        &mut self,
        admin: &Credential,
        scope: &Scope,
        channel: &str,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        require_admin(&mut tx, &self.spec, admin)?;
        // Revocation remains possible when paid writes are suspended.
        let owner = authority(&mut tx, &self.spec, admin, &current, false)?;
        let time = integer(now(&mut tx)?)?;
        if tx.execute("UPDATE alt_link_channels SET enabled=false,disabled_at=$4 WHERE repository=$1 AND epoch=$2 AND channel=$3 AND enabled", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&channel,&time])?==1 {
            crate::browser::audit_details(&mut tx,&owner.actor_id,"change_link.disabled",&scope.repository_instance_id,&serde_json::json!({"channel":channel}))?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}
