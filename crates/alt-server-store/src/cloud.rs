//! Persisted platform authority. HTTP and encryption belong to the caller; this
//! module validates leases again against the database clock inside each use.
mod held;

use crate::Jsonb;
use crate::database::Transaction;
use crate::store::{instance, integer, now, token_digest, unsigned};
use crate::{ByteStore, Credential, Error, InstanceSpec, Result, Store};
use alt_crypto::ed25519::SigningKey;
use alt_json_serde::Serde;
use alt_pg::Client;
use alt_remote_protocol::{ActorKind, Attribution, DeploymentMode, Limits};
use golia_platform_contract::{
    AccountAuthorization, AltServiceAuthorization, Audience, Capability, Product,
};
pub(crate) use held::check_instance;
use held::{binding, check_revision};
mod service;
pub(crate) use service::{repository_quota, service, writes_enabled};
mod restore;
pub use restore::CloudRestoreBinding;
pub(crate) use restore::restore_binding;

/// Authenticated ciphertext, never the upstream bearer grant. No Debug output.
pub struct CloudRefresh {
    pub credential_digest: String,
    pub sealed_grant: String,
    pub claims: AccountAuthorization,
    pub browser: bool,
    pub refresh_needed: bool,
    /// whether the lease is good as it is, for the time it has left
    pub serves: bool,
}
/// Whether a lease is to be renewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renewal {
    /// not yet
    Fresh,
    /// it nears its end and is good until then
    Soon,
    /// it is over, or was never given, or is no longer the one that counts
    Due,
}
/// Stable local identifier for authenticated grant encryption; not a bearer.
pub fn cloud_credential_digest(token: &str) -> Result<String> {
    crate::store::validate_token(token)?;
    Ok(token_digest(token))
}
fn denied<T>(result: golia_platform_contract::Result<T>) -> Result<T> {
    result.map_err(|_| Error::Unauthorized)
}
fn audience_matches(spec: &InstanceSpec, audience: &Audience) -> Result<()> {
    denied(audience.validate())?;
    if spec.deployment_mode != DeploymentMode::Cloud
        || audience.product != Product::Alt
        || audience.installation_id.as_str() != spec.installation_id
        || audience.workspace_id.as_str() != spec.workspace_id
        || audience.generation.0 != spec.runtime_generation
    {
        return Err(Error::Precondition("Cloud installation audience"));
    }
    Ok(())
}
pub(crate) fn initialize_binding(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    audience: &Audience,
    key_digest: &str,
    provision_digest: Option<&str>,
) -> Result<()> {
    audience_matches(spec, audience)?;
    crate::bytes::check_digest(key_digest)?;
    if let Some(digest) = provision_digest {
        crate::bytes::check_digest(digest)?;
    }
    tx.execute(
        "INSERT INTO alt_cloud_binding(singleton,audience,runtime_key_digest,provision_digest) VALUES(true,$1,$2,$3)",
        &[&Jsonb::from_serde(&audience)?, &key_digest, &provision_digest],
    )?;
    tx.execute(
        "INSERT INTO alt_browser_binding(singleton,public_origin) VALUES(true,$1)",
        &[&audience.origin.as_str()],
    )?;
    Ok(())
}
fn check_claims(tx: &mut Transaction<'_>, claims: &AccountAuthorization) -> Result<()> {
    let row = binding(tx)?;
    let audience: Audience = row.get::<_, Jsonb>("audience").read_serde()?;
    denied(claims.check(&audience, now(tx)?))?;
    if !claims.capabilities.contains(&Capability::UseProduct) {
        return Err(Error::Unauthorized);
    }
    check_revision(tx, claims.lease.revocation_generation.0)
}
pub(crate) fn ciphertext(value: &str) -> Result<()> {
    // This is an envelope bound, not a cryptographic check: the owning server
    // supplies authenticated ciphertext and decrypts before contacting control.
    if value.is_empty()
        || value.len() > 512
        || !value.is_ascii()
        || value.chars().any(char::is_control)
    {
        return Err(Error::Precondition("sealed platform grant envelope"));
    }
    Ok(())
}
fn claims_for(tx: &mut Transaction<'_>, digest: &str) -> Result<AccountAuthorization> {
    let row = tx
        .query_opt(
            "SELECT claims FROM alt_cloud_credentials WHERE token_digest=$1",
            &[&digest],
        )?
        .ok_or(Error::Unauthorized)?;
    Ok(row.get::<_, Jsonb>(0).read_serde()?)
}
pub(crate) fn check_credential(
    tx: &mut Transaction<'_>,
    _spec: &InstanceSpec,
    credential: &Credential,
    row: &alt_pg::Row,
) -> Result<()> {
    let claims = claims_for(tx, &credential.digest)?;
    check_claims(tx, &claims)?;
    let member = tx
        .query_opt(
            "SELECT membership_id FROM alt_cloud_accounts WHERE actor_id=$1",
            &[&claims.account_id.as_str()],
        )?
        .ok_or(Error::Unauthorized)?;
    if claims.account_id.as_str() != row.get::<_, String>("actor_id")
        || claims.membership_id.as_str() != member.get::<_, String>(0)
        || row.get::<_, bool>("administrator")
            != claims.capabilities.contains(&Capability::ManageInstance)
    {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
impl Store {
    /// Physical initialization. The supplied key digest is derived from the
    /// runtime credential; it is not an alternative credential for local login.
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_cloud(
        client: Client,
        bytes: ByteStore,
        spec: InstanceSpec,
        signing_key: SigningKey,
        limits: Limits,
        audience: &Audience,
        runtime_key_digest: &str,
    ) -> Result<Self> {
        audience_matches(&spec, audience)?;
        Self::initialize_bound(
            client,
            bytes,
            spec,
            signing_key,
            limits,
            Some((audience, runtime_key_digest, None)),
        )
    }
    /// Provisioned instances bind the immutable executor specification in the
    /// same transaction that creates the database identity.
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_cloud_provisioned(
        client: Client,
        bytes: ByteStore,
        spec: InstanceSpec,
        signing_key: SigningKey,
        limits: Limits,
        audience: &Audience,
        runtime_key_digest: &str,
        provision_digest: &str,
    ) -> Result<Self> {
        audience_matches(&spec, audience)?;
        Self::initialize_bound(
            client,
            bytes,
            spec,
            signing_key,
            limits,
            Some((audience, runtime_key_digest, Some(provision_digest))),
        )
    }
    pub fn check_cloud_provision(&mut self, expected: Option<&str>) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let actual: Option<String> = binding(&mut tx)?.get("provision_digest");
        if actual.as_deref() != expected {
            return Err(Error::Precondition("Cloud provision binding"));
        }
        tx.commit()?;
        Ok(())
    }
    /// A write/commit plus a durable filesystem round trip, with fresh volume
    /// identity checking. No content permissions or control-plane lease needed.
    pub fn cloud_readiness(&mut self, digest: &str) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        check_instance(&mut tx, &self.spec)?;
        if binding(&mut tx)?
            .get::<_, Option<String>>("provision_digest")
            .as_deref()
            != Some(digest)
        {
            return Err(Error::Precondition("Cloud provision binding"));
        }
        self.bytes.readiness(
            &self.spec,
            alt_hash::blake3::hash(self.signing_key.verifying_key().as_bytes())
                .to_hex()
                .as_str(),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn check_cloud_binding(
        &mut self,
        audience: &Audience,
        runtime_key_digest: &str,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        audience_matches(&self.spec, audience)?;
        let row = binding(&mut tx)?;
        denied(audience.check(&row.get::<_, Jsonb>("audience").read_serde()?))?;
        if row.get::<_, String>("runtime_key_digest") != runtime_key_digest {
            return Err(Error::Unauthorized);
        }
        tx.commit()?;
        Ok(())
    }
    /// Called only after a successful platform ticket exchange. Local bearer
    /// expiry is bounded separately; upstream authority must still refresh at
    /// least once per minute. Existing repository grants are never inferred.
    pub fn accept_cloud_login(
        &mut self,
        token: &str,
        sealed_grant: &str,
        claims: &AccountAuthorization,
        expires_at: u64,
        browser: bool,
    ) -> Result<()> {
        self.accept_cloud_login_inner(token, sealed_grant, claims, expires_at, browser, None)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accept_cloud_login_inner(
        &mut self,
        token: &str,
        sealed_grant: &str,
        claims: &AccountAuthorization,
        expires_at: u64,
        browser: bool,
        flow: Option<(&str, &str)>,
    ) -> Result<()> {
        ciphertext(sealed_grant)?;
        let digest = token_digest(token);
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        if let Some((id, ticket_digest)) = flow {
            crate::cloud_login::check_completion(&mut tx, id, ticket_digest, &digest, expires_at)?;
        }
        check_claims(&mut tx, claims)?;
        if expires_at > now(&mut tx)?.saturating_add(8 * 3_600_000) {
            return Err(Error::Precondition("Cloud credential lifetime"));
        }
        if let Some(row) = tx.query_opt(
            "SELECT membership_id FROM alt_cloud_accounts WHERE actor_id=$1",
            &[&claims.account_id.as_str()],
        )? && row.get::<_, String>(0) != claims.membership_id.as_str()
        {
            // Removing and rejoining an organization is a new authority. Do not
            // revive product ACLs or credentials attached to the old member.
            tx.execute("UPDATE alt_repositories SET revocation_generation=revocation_generation+1 WHERE id IN (SELECT repository FROM alt_grants WHERE actor_id=$1)", &[&claims.account_id.as_str()])?;
            tx.execute(
                "DELETE FROM alt_grants WHERE actor_id=$1",
                &[&claims.account_id.as_str()],
            )?;
            tx.execute(
                "DELETE FROM alt_review_grants WHERE actor_id=$1",
                &[&claims.account_id.as_str()],
            )?;
            tx.execute(
                "UPDATE alt_credentials SET revoked=true,revoked_at=COALESCE(revoked_at,floor(extract(epoch FROM clock_timestamp())*1000)::bigint) WHERE actor_id=$1",
                &[&claims.account_id.as_str()],
            )?;
        }
        tx.execute("INSERT INTO alt_cloud_accounts(actor_id,membership_id) VALUES($1,$2) ON CONFLICT(actor_id) DO UPDATE SET membership_id=excluded.membership_id", &[&claims.account_id.as_str(), &claims.membership_id.as_str()])?;
        crate::store::insert_known_token(
            &mut tx,
            token,
            claims.account_id.as_str(),
            &Attribution {
                kind: ActorKind::Human,
                on_behalf_of: None,
                delegation_id: None,
                agent_session: None,
            },
            DeploymentMode::Cloud,
            expires_at,
            claims.capabilities.contains(&Capability::ManageInstance),
        )?;
        let value = Jsonb::from_serde(&claims)?;
        let created = integer(now(&mut tx)?)?;
        tx.execute("INSERT INTO alt_cloud_credentials(token_digest,claims,sealed_grant,browser,created_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(token_digest) DO NOTHING", &[&digest,&value,&sealed_grant,&browser,&created])?;
        let row = tx.query_one(
            "SELECT claims,sealed_grant,browser FROM alt_cloud_credentials WHERE token_digest=$1",
            &[&digest],
        )?;
        if row.get::<_, Jsonb>(0) != value
            || row.get::<_, String>(1) != sealed_grant
            || row.get::<_, bool>(2) != browser
        {
            return Err(Error::Precondition("Cloud login replay differs"));
        }
        if let Some((id, _)) = flow {
            tx.execute(
                "UPDATE alt_cloud_logins SET state='complete' WHERE id=$1",
                &[&id],
            )?;
        }
        crate::browser::audit_details(
            &mut tx,
            claims.account_id.as_str(),
            "cloud.login",
            &digest,
            &Jsonb(
                alt_json::json!({"session_id": Serde(&claims.session_id), "membership_id":Serde(&claims.membership_id), "browser":browser}),
            ),
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    /// Returns a refresh descriptor even when the short authority lease expired;
    /// the local bearer must still exist, be unrevoked and within its lifetime.
    pub fn cloud_refresh(&mut self, token: &str) -> Result<CloudRefresh> {
        crate::store::validate_token(token)?;
        let digest = token_digest(token);
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        let local = tx.query_opt("SELECT expires_at,revoked,identity_source FROM alt_credentials WHERE token_digest=$1", &[&digest])?.ok_or(Error::Unauthorized)?;
        if local.get::<_, bool>("revoked")
            || local.get::<_, String>("identity_source") != "cloud"
            || unsigned(local.get("expires_at"))? <= now(&mut tx)?
        {
            return Err(Error::Unauthorized);
        }
        let parent = crate::delegation::parent(&mut tx, &digest)?;
        let upstream = parent.as_deref().unwrap_or(&digest);
        let row = tx.query_opt("SELECT c.expires_at,c.revoked,p.claims,p.sealed_grant,p.browser FROM alt_credentials c JOIN alt_cloud_credentials p USING(token_digest) WHERE token_digest=$1", &[&upstream])?.ok_or(Error::Unauthorized)?;
        if row.get::<_, bool>("revoked") || unsigned(row.get("expires_at"))? <= now(&mut tx)? {
            return Err(Error::Unauthorized);
        }
        let claims: AccountAuthorization = row.get::<_, Jsonb>("claims").read_serde()?;
        let authority = binding(&mut tx)?;
        let audience: Audience = authority.get::<_, Jsonb>("audience").read_serde()?;
        let time = now(&mut tx)?;
        let stale =
            claims.lease.revocation_generation.0 < unsigned(authority.get("authority_revision"))?;
        let near = claims
            .check(&audience, time.saturating_add(10_000))
            .is_err();
        let refresh_needed = near || stale;
        let serves = !stale && claims.check(&audience, time).is_ok();
        let result = CloudRefresh {
            credential_digest: upstream.to_owned(),
            sealed_grant: row.get("sealed_grant"),
            claims,
            refresh_needed,
            serves,
            browser: parent.is_none() && row.get::<_, bool>("browser"),
        };
        tx.commit()?;
        Ok(result)
    }
    pub fn refresh_cloud_account(
        &mut self,
        credential_digest: &str,
        claims: &AccountAuthorization,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        check_claims(&mut tx, claims)?;
        let previous = claims_for(&mut tx, credential_digest)?;
        denied(claims.check_after(&previous))?;
        // A refresh cannot turn an old membership's local token into a new one.
        if claims.membership_id != previous.membership_id {
            return Err(Error::Unauthorized);
        }
        let time = integer(now(&mut tx)?)?;
        if tx.execute("UPDATE alt_credentials SET administrator=$2 WHERE token_digest=$1 AND NOT revoked AND expires_at>$3", &[&credential_digest, &claims.capabilities.contains(&Capability::ManageInstance), &time])? != 1 {
            return Err(Error::Unauthorized);
        }
        tx.execute(
            "UPDATE alt_cloud_credentials SET claims=$2 WHERE token_digest=$1",
            &[&credential_digest, &Jsonb::from_serde(&claims)?],
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    /// Persist a definitive provider refusal. Network errors must not call this
    /// or renew any lease; they may use only an already-valid cached authority.
    pub fn revoke_cloud_credential(&mut self, credential_digest: &str) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        tx.execute("UPDATE alt_credentials SET revoked=true,revoked_at=COALESCE(revoked_at,floor(extract(epoch FROM clock_timestamp())*1000)::bigint) WHERE token_digest=$1 AND identity_source='cloud'", &[&credential_digest])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    /// Invalidate known-refused service authority without losing its monotonic
    /// history. A delayed response issued before refusal cannot restore writes.
    pub fn revoke_cloud_service(&mut self) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        let time = integer(now(&mut tx)?)?;
        tx.execute(
            "UPDATE alt_cloud_binding SET service_denied_at=$1 WHERE singleton",
            &[&time],
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    pub fn cloud_service_renewal(&mut self) -> Result<Renewal> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        let row = binding(&mut tx)?;
        let audience: Audience = row.get::<_, Jsonb>("audience").read_serde()?;
        let needed = match row.get::<_, Option<Jsonb>>("service") {
            Some(value) => {
                let service: AltServiceAuthorization = value.read_serde()?;
                let time = now(&mut tx)?;
                let stale = service.lease.revocation_generation.0
                    < unsigned(row.get("authority_revision"))?
                    || service.lease.issued_at_unix_ms.0 <= unsigned(row.get("service_denied_at"))?;
                if stale || service.check(&audience, time).is_err() {
                    Renewal::Due
                } else if service
                    .check(&audience, time.saturating_add(10_000))
                    .is_err()
                {
                    Renewal::Soon
                } else {
                    Renewal::Fresh
                }
            }
            None => Renewal::Due,
        };
        tx.commit()?;
        Ok(needed)
    }
    pub fn refresh_cloud_service(&mut self, service: &AltServiceAuthorization) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        let row = binding(&mut tx)?;
        let audience = row.get::<_, Jsonb>("audience").read_serde()?;
        denied(service.check(&audience, now(&mut tx)?))?;
        if service.lease.issued_at_unix_ms.0 <= unsigned(row.get("service_denied_at"))? {
            return Err(Error::Unauthorized);
        }
        if let Some(value) = row.get::<_, Option<Jsonb>>("service") {
            denied(service.check_after(&value.read_serde()?))?;
        }
        check_revision(&mut tx, service.lease.revocation_generation.0)?;
        tx.execute(
            "UPDATE alt_cloud_binding SET service=$1 WHERE singleton",
            &[&Jsonb::from_serde(&service)?],
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}
