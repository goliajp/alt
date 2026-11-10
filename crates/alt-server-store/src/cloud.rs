//! Persisted platform authority. HTTP and encryption belong to the caller; this
//! module validates leases again against the database clock inside each use.
use crate::database::Transaction;
use crate::store::{instance, integer, now, token_digest, unsigned};
use crate::{ByteStore, Credential, Error, InstanceSpec, Result, Store};
use alt_remote_protocol::{ActorKind, Attribution, DeploymentMode, Limits};
use ed25519_dalek::SigningKey;
use golia_platform_contract::{
    AccountAuthorization, AltServiceAuthorization, Audience, Capability, Product,
};
use postgres::Client;

/// Authenticated ciphertext, never the upstream bearer grant. No Debug output.
pub struct CloudRefresh {
    pub credential_digest: String,
    pub sealed_grant: String,
    pub claims: AccountAuthorization,
    pub browser: bool,
    pub refresh_needed: bool,
}
/// Fresh platform authority for an externally fenced disaster recovery. Content
/// and membership history survive; runtime credentials and leases do not.
pub struct CloudRestoreBinding<'a> {
    pub audience: &'a Audience,
    pub runtime_key_digest: &'a str,
    pub provision_digest: Option<&'a str>,
}
pub(crate) fn restore_binding(
    tx: &mut Transaction<'_>,
    source: &InstanceSpec,
    generation: u64,
    replacement: &CloudRestoreBinding<'_>,
) -> Result<()> {
    let row = binding(tx)?;
    let mut expected: Audience = serde_json::from_value(row.get("audience"))?;
    audience_matches(source, &expected)?;
    expected.generation = golia_platform_contract::Counter(generation);
    let mut target = source.clone();
    target.runtime_generation = generation;
    audience_matches(&target, replacement.audience)?;
    crate::bytes::check_digest(replacement.runtime_key_digest)?;
    if let Some(digest) = replacement.provision_digest {
        crate::bytes::check_digest(digest)?;
    }
    let old_provision: Option<String> = row.get("provision_digest");
    if replacement.audience != &expected
        || replacement.runtime_key_digest == row.get::<_, String>("runtime_key_digest")
        || old_provision
            .is_some_and(|old| replacement.provision_digest.is_none_or(|new| new == old))
    {
        return Err(Error::Precondition(
            "Cloud recovery requires same identity and fresh runtime binding",
        ));
    }
    tx.execute(
        "UPDATE alt_cloud_binding SET audience=$1,runtime_key_digest=$2,provision_digest=$3,service=NULL,service_denied_at=0 WHERE singleton",
        &[&serde_json::to_value(replacement.audience)?, &replacement.runtime_key_digest, &replacement.provision_digest],
    )?;
    Ok(())
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
        &[&serde_json::to_value(audience)?, &key_digest, &provision_digest],
    )?;
    tx.execute(
        "INSERT INTO alt_browser_binding(singleton,public_origin) VALUES(true,$1)",
        &[&audience.origin.as_str()],
    )?;
    Ok(())
}
/// This barrier precedes repository and credential locks. It serializes cache
/// installation, revocation, quota accounting and authority use across workers.
/// A backwards wall clock never gives a previously accepted lease extra life.
pub(crate) fn check_instance(tx: &mut Transaction<'_>, spec: &InstanceSpec) -> Result<()> {
    let row = binding(tx)?;
    let audience: Audience = serde_json::from_value(row.get("audience"))?;
    audience_matches(spec, &audience)?;
    let time = now(tx)?;
    if time < unsigned(row.get("clock_high_water"))? {
        return Err(Error::Precondition("Cloud authority clock moved backwards"));
    }
    tx.execute(
        "UPDATE alt_cloud_binding SET clock_high_water=$1 WHERE singleton",
        &[&integer(time)?],
    )?;
    Ok(())
}
fn binding(tx: &mut Transaction<'_>) -> Result<postgres::Row> {
    tx.query_opt("SELECT audience,runtime_key_digest,provision_digest,clock_high_water,authority_revision,service,service_denied_at FROM alt_cloud_binding WHERE singleton FOR UPDATE", &[])?
        .ok_or(Error::Precondition("Cloud platform binding required"))
}
fn check_revision(tx: &mut Transaction<'_>, revision: u64) -> Result<()> {
    let row = binding(tx)?;
    if revision < unsigned(row.get("authority_revision"))? {
        return Err(Error::Unauthorized);
    }
    tx.execute(
        "UPDATE alt_cloud_binding SET authority_revision=$1 WHERE singleton",
        &[&integer(revision)?],
    )?;
    Ok(())
}
fn check_claims(tx: &mut Transaction<'_>, claims: &AccountAuthorization) -> Result<()> {
    let row = binding(tx)?;
    let audience: Audience = serde_json::from_value(row.get("audience"))?;
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
    Ok(serde_json::from_value(row.get(0))?)
}
pub(crate) fn check_credential(
    tx: &mut Transaction<'_>,
    _spec: &InstanceSpec,
    credential: &Credential,
    row: &postgres::Row,
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
            blake3::hash(self.signing_key.verifying_key().as_bytes())
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
        denied(audience.check(&serde_json::from_value(row.get("audience"))?))?;
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
                "UPDATE alt_credentials SET revoked=true WHERE actor_id=$1",
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
        let value = serde_json::to_value(claims)?;
        let created = integer(now(&mut tx)?)?;
        tx.execute("INSERT INTO alt_cloud_credentials(token_digest,claims,sealed_grant,browser,created_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(token_digest) DO NOTHING", &[&digest,&value,&sealed_grant,&browser,&created])?;
        let row = tx.query_one(
            "SELECT claims,sealed_grant,browser FROM alt_cloud_credentials WHERE token_digest=$1",
            &[&digest],
        )?;
        if row.get::<_, serde_json::Value>(0) != value
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
            &serde_json::json!({"session_id": claims.session_id, "membership_id":claims.membership_id, "browser":browser}),
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
        let claims: AccountAuthorization = serde_json::from_value(row.get("claims"))?;
        let authority = binding(&mut tx)?;
        let audience: Audience = serde_json::from_value(authority.get("audience"))?;
        let refresh_needed = claims
            .check(&audience, now(&mut tx)?.saturating_add(10_000))
            .is_err()
            || claims.lease.revocation_generation.0
                < unsigned(authority.get("authority_revision"))?;
        let result = CloudRefresh {
            credential_digest: upstream.to_owned(),
            sealed_grant: row.get("sealed_grant"),
            claims,
            refresh_needed,
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
            &[&credential_digest, &serde_json::to_value(claims)?],
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
        tx.execute("UPDATE alt_credentials SET revoked=true WHERE token_digest=$1 AND identity_source='cloud'", &[&credential_digest])?;
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
    pub fn cloud_service_refresh_needed(&mut self) -> Result<bool> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        let row = binding(&mut tx)?;
        let audience: Audience = serde_json::from_value(row.get("audience"))?;
        let needed = match row.get::<_, Option<serde_json::Value>>("service") {
            Some(value) => {
                let service: AltServiceAuthorization = serde_json::from_value(value)?;
                service
                    .check(&audience, now(&mut tx)?.saturating_add(10_000))
                    .is_err()
                    || service.lease.revocation_generation.0
                        < unsigned(row.get("authority_revision"))?
                    || service.lease.issued_at_unix_ms.0 <= unsigned(row.get("service_denied_at"))?
            }
            None => true,
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
        let audience = serde_json::from_value(row.get("audience"))?;
        denied(service.check(&audience, now(&mut tx)?))?;
        if service.lease.issued_at_unix_ms.0 <= unsigned(row.get("service_denied_at"))? {
            return Err(Error::Unauthorized);
        }
        if let Some(value) = row.get::<_, Option<serde_json::Value>>("service") {
            denied(service.check_after(&serde_json::from_value(value)?))?;
        }
        check_revision(&mut tx, service.lease.revocation_generation.0)?;
        tx.execute(
            "UPDATE alt_cloud_binding SET service=$1 WHERE singleton",
            &[&serde_json::to_value(service)?],
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}

/// Account authorization and service authorization are deliberately separate.
/// Missing/expired paid service disables new content without blocking exports.
pub(crate) fn service(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
) -> Result<Option<AltServiceAuthorization>> {
    if spec.deployment_mode != DeploymentMode::Cloud {
        return Ok(None);
    }
    let row = binding(tx)?;
    let value: serde_json::Value =
        row.get::<_, Option<serde_json::Value>>("service")
            .ok_or(Error::Precondition(
                "Cloud service authorization unavailable",
            ))?;
    let service: AltServiceAuthorization = serde_json::from_value(value)?;
    let audience: Audience = serde_json::from_value(row.get("audience"))?;
    denied(service.check(&audience, now(tx)?))?;
    if service.lease.revocation_generation.0 < unsigned(row.get("authority_revision"))?
        || service.lease.issued_at_unix_ms.0 <= unsigned(row.get("service_denied_at"))?
        || !service.writes_enabled
    {
        return Err(Error::Precondition(
            "Cloud writes are not currently authorized",
        ));
    }
    Ok(Some(service))
}
pub(crate) fn writes_enabled(tx: &mut Transaction<'_>, spec: &InstanceSpec) -> Result<bool> {
    if spec.deployment_mode != DeploymentMode::Cloud {
        return Ok(true);
    }
    match service(tx, spec) {
        Ok(_) => Ok(true),
        Err(Error::Precondition(_) | Error::Unauthorized) => Ok(false),
        Err(error) => Err(error),
    }
}
pub(crate) fn repository_quota(tx: &mut Transaction<'_>, spec: &InstanceSpec) -> Result<()> {
    if let Some(service) = service(tx, spec)? {
        let count: i64 = tx
            .query_one("SELECT count(*) FROM alt_repositories", &[])?
            .get(0);
        if unsigned(count)? >= service.limits.repositories.0 {
            return Err(Error::Limit("Cloud repository quota"));
        }
    }
    Ok(())
}
