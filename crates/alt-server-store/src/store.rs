use crate::{ByteStore, Error, Result};
use alt_remote_protocol::{Attribution, DeploymentMode, Limits, ObjectFormat, Scope};
use alt_repository_service::{Authorization, Grant, RefPolicy, RepositorySnapshot};
use ed25519_dalek::SigningKey;
use postgres::{Client, Transaction};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSpec {
    pub installation_id: String,
    pub workspace_id: String,
    pub deployment_mode: DeploymentMode,
    pub runtime_generation: u64,
}
impl InstanceSpec {
    fn validate(&self) -> Result<()> {
        Scope {
            installation_id: self.installation_id.clone(),
            repository_instance_id: self.workspace_id.clone(),
            recovery_epoch: self.runtime_generation,
        }
        .validate()?;
        integer(self.runtime_generation)?;
        Ok(())
    }
    fn mode(&self) -> &'static str {
        mode(self.deployment_mode)
    }
}
/// An authenticated handle contains no token secret and cannot be constructed
/// by callers. Every operation rechecks the credential in the DB transaction.
#[derive(Clone)]
pub struct Credential {
    pub(crate) digest: String,
}

pub struct Store {
    pub(crate) client: Client,
    pub(crate) bytes: ByteStore,
    pub(crate) spec: InstanceSpec,
    pub(crate) signing_key: SigningKey,
    pub(crate) limits: Limits,
}
impl Store {
    /// Explicit first initialization, never invoked as a fallback from open.
    /// The caller supplies a dedicated database connection and persisted key.
    pub fn initialize(
        mut client: Client,
        bytes: ByteStore,
        spec: InstanceSpec,
        signing_key: SigningKey,
        limits: Limits,
    ) -> Result<Self> {
        spec.validate()?;
        limits.validate()?;
        let mut tx = client.transaction()?;
        tx.query_one("SELECT pg_advisory_xact_lock(715071909111)", &[])?;
        tx.batch_execute("CREATE TABLE IF NOT EXISTS alt_schema(version integer PRIMARY KEY, checksum text NOT NULL)")?;
        if !tx.query("SELECT version FROM alt_schema", &[])?.is_empty() {
            return Err(Error::Precondition(
                "instance already initialized; use open",
            ));
        }
        tx.batch_execute(include_str!("schema.sql"))?;
        let checksum = blake3::hash(include_bytes!("schema.sql"))
            .to_hex()
            .to_string();
        tx.execute("INSERT INTO alt_schema VALUES (1,$1)", &[&checksum])?;
        let key_id = blake3::hash(signing_key.verifying_key().as_bytes())
            .to_hex()
            .to_string();
        tx.execute("INSERT INTO alt_instance(singleton,installation_id,workspace_id,deployment_mode,runtime_generation,signing_key_id) VALUES(true,$1,$2,$3,$4,$5)", &[&spec.installation_id,&spec.workspace_id,&spec.mode(),&integer(spec.runtime_generation)?,&key_id])?;
        crate::schema::apply_v2(&mut tx)?;
        crate::schema::apply_v3(&mut tx)?;
        tx.commit()?;
        Self::open(client, bytes, spec, signing_key, limits)
    }
    pub fn open(
        mut client: Client,
        bytes: ByteStore,
        spec: InstanceSpec,
        signing_key: SigningKey,
        limits: Limits,
    ) -> Result<Self> {
        spec.validate()?;
        limits.validate()?;
        crate::schema::check(&mut client, 3)?;
        let mut tx = client.transaction()?;
        instance(&mut tx, &spec, &signing_key)?;
        tx.commit()?;
        Ok(Self {
            client,
            bytes,
            spec,
            signing_key,
            limits,
        })
    }
    pub fn spec(&self) -> &InstanceSpec {
        &self.spec
    }
    pub fn bytes(&self) -> &ByteStore {
        &self.bytes
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
    /// Local operator bootstrap. It works only for a self-hosted installation
    /// with no credential rows, and closes permanently after the first token.
    pub fn bootstrap_admin(&mut self, actor: &str, expires_at_unix_ms: u64) -> Result<String> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        tx.batch_execute("LOCK TABLE alt_credentials IN EXCLUSIVE MODE")?;
        if self.spec.deployment_mode != DeploymentMode::SelfHosted
            || !tx
                .query("SELECT 1 FROM alt_credentials LIMIT 1", &[])?
                .is_empty()
        {
            return Err(Error::Unauthorized);
        }
        let attribution = Attribution {
            kind: alt_remote_protocol::ActorKind::Human,
            on_behalf_of: None,
            delegation_id: None,
            agent_session: None,
        };
        let token = insert_token(
            &mut tx,
            actor,
            &attribution,
            self.spec.deployment_mode,
            expires_at_unix_ms,
            true,
        )?;
        tx.commit()?;
        Ok(token)
    }
    /// Explicit operator initialization with a secret already fsynced by the
    /// caller. Exact replay is allowed; a failed response cannot strand an
    /// installation with an unknowable randomly generated first credential.
    pub fn bootstrap_admin_with_token(
        &mut self,
        actor: &str,
        expires_at_unix_ms: u64,
        token: &str,
    ) -> Result<()> {
        validate_token(token)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        tx.batch_execute("LOCK TABLE alt_credentials IN EXCLUSIVE MODE")?;
        if self.spec.deployment_mode != DeploymentMode::SelfHosted {
            return Err(Error::Unauthorized);
        }
        let attribution = Attribution {
            kind: alt_remote_protocol::ActorKind::Human,
            on_behalf_of: None,
            delegation_id: None,
            agent_session: None,
        };
        let rows = tx.query("SELECT token_digest FROM alt_credentials LIMIT 2", &[])?;
        if !rows.is_empty()
            && (rows.len() != 1 || rows[0].get::<_, String>(0) != token_digest(token))
        {
            return Err(Error::Unauthorized);
        }
        insert_known_token(
            &mut tx,
            token,
            actor,
            &attribution,
            self.spec.deployment_mode,
            expires_at_unix_ms,
            true,
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    pub fn authenticate(&mut self, token: &str) -> Result<Credential> {
        if token.len() != 69 || !token.starts_with("alt1_") {
            return Err(Error::Unauthorized);
        }
        let digest = token_digest(token);
        let credential = Credential { digest };
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        credential_row(&mut tx, &self.spec, &credential)?;
        tx.commit()?;
        Ok(credential)
    }
    pub fn issue_local_token(
        &mut self,
        admin: &Credential,
        actor: &str,
        attribution: &Attribution,
        expires_at_unix_ms: u64,
        administrator: bool,
    ) -> Result<String> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        if self.spec.deployment_mode != DeploymentMode::SelfHosted {
            return Err(Error::Unauthorized);
        }
        let token = insert_token(
            &mut tx,
            actor,
            attribution,
            self.spec.deployment_mode,
            expires_at_unix_ms,
            administrator,
        )?;
        tx.commit()?;
        Ok(token)
    }
    /// Register a separately generated, privately persisted operator secret.
    /// Reuse with different claims is rejected rather than rotating authority.
    pub fn issue_local_token_with_secret(
        &mut self,
        admin: &Credential,
        token: &str,
        actor: &str,
        attribution: &Attribution,
        expires_at_unix_ms: u64,
        administrator: bool,
    ) -> Result<()> {
        validate_token(token)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        if self.spec.deployment_mode != DeploymentMode::SelfHosted {
            return Err(Error::Unauthorized);
        }
        insert_known_token(
            &mut tx,
            token,
            actor,
            attribution,
            self.spec.deployment_mode,
            expires_at_unix_ms,
            administrator,
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    pub fn create_repository(
        &mut self,
        admin: &Credential,
        repository: &str,
        quota_bytes: u64,
    ) -> Result<Scope> {
        let scope = Scope {
            installation_id: self.spec.installation_id.clone(),
            repository_instance_id: repository.into(),
            recovery_epoch: 1,
        };
        scope.validate()?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        tx.execute("INSERT INTO alt_repositories(id,recovery_epoch,object_format,quota_bytes) VALUES($1,1,'sha1',$2)",&[&repository,&integer(quota_bytes)?])?;
        tx.commit()?;
        Ok(scope)
    }
    pub fn set_grants(
        &mut self,
        admin: &Credential,
        repository: &str,
        actor: &str,
        can_read: bool,
        publish_records: bool,
        grants: &[Grant],
    ) -> Result<()> {
        for grant in grants {
            grant.validate()?;
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let _ = snapshot(&mut tx, &self.spec, repository)?;
        require_admin(&mut tx, &self.spec, admin)?;
        tx.execute("INSERT INTO alt_grants(actor_id,repository,can_read,publish_records,grants) VALUES($1,$2,$3,$4,$5) ON CONFLICT(actor_id,repository) DO UPDATE SET can_read=excluded.can_read,publish_records=excluded.publish_records,grants=excluded.grants",&[&actor,&repository,&can_read,&publish_records,&serde_json::to_value(grants)?])?;
        tx.execute(
            "UPDATE alt_repositories SET revocation_generation=revocation_generation+1 WHERE id=$1",
            &[&repository],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn set_policies(
        &mut self,
        admin: &Credential,
        repository: &str,
        policies: &[RefPolicy],
    ) -> Result<()> {
        for policy in policies {
            policy.validate()?;
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let _ = snapshot(&mut tx, &self.spec, repository)?;
        require_admin(&mut tx, &self.spec, admin)?;
        tx.execute(
            "UPDATE alt_repositories SET policies=$2,policy_version=policy_version+1 WHERE id=$1",
            &[&repository, &serde_json::to_value(policies)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn revoke(&mut self, admin: &Credential, token: &str) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        tx.execute(
            "UPDATE alt_credentials SET revoked=true WHERE token_digest=$1",
            &[&token_digest(token)],
        )?;
        tx.commit()?;
        Ok(())
    }
}
pub(crate) fn integer(value: u64) -> Result<i64> {
    value
        .try_into()
        .map_err(|_| Error::Limit("database integer range"))
}
pub(crate) fn unsigned(value: i64) -> Result<u64> {
    value
        .try_into()
        .map_err(|_| Error::Integrity("negative database counter"))
}
pub(crate) fn now(tx: &mut Transaction<'_>) -> Result<u64> {
    unsigned(
        tx.query_one(
            "SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint",
            &[],
        )?
        .get(0),
    )
}
pub(crate) fn instance(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &SigningKey,
) -> Result<()> {
    let row=tx.query_opt("SELECT installation_id,workspace_id,deployment_mode,runtime_generation,signing_key_id FROM alt_instance WHERE singleton FOR SHARE",&[])?.ok_or(Error::Precondition("instance not initialized"))?;
    if row.get::<_, String>(0) != spec.installation_id
        || row.get::<_, String>(1) != spec.workspace_id
        || row.get::<_, String>(2) != spec.mode()
        || unsigned(row.get(3))? != spec.runtime_generation
        || row.get::<_, String>(4)
            != blake3::hash(key.verifying_key().as_bytes())
                .to_hex()
                .as_str()
    {
        return Err(Error::Precondition(
            "instance identity, mode, generation or signing key",
        ));
    }
    Ok(())
}
fn mode(mode: DeploymentMode) -> &'static str {
    match mode {
        DeploymentMode::SelfHosted => "self_hosted",
        DeploymentMode::Cloud => "cloud",
    }
}
fn token_digest(token: &str) -> String {
    blake3::derive_key("jp.golia.alt.server-token.v1", token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn insert_token(
    tx: &mut Transaction<'_>,
    actor: &str,
    attribution: &Attribution,
    source: DeploymentMode,
    expires: u64,
    administrator: bool,
) -> Result<String> {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    let token = format!(
        "alt1_{}",
        secret
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    insert_known_token(
        tx,
        &token,
        actor,
        attribution,
        source,
        expires,
        administrator,
    )?;
    Ok(token)
}
fn validate_token(token: &str) -> Result<()> {
    if token.len() != 69
        || !token.starts_with("alt1_")
        || !token.as_bytes()[5..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn insert_known_token(
    tx: &mut Transaction<'_>,
    token: &str,
    actor: &str,
    attribution: &Attribution,
    source: DeploymentMode,
    expires: u64,
    administrator: bool,
) -> Result<()> {
    validate_token(token)?;
    Scope {
        installation_id: actor.into(),
        repository_instance_id: "actor".into(),
        recovery_epoch: 1,
    }
    .validate()?;
    attribution.validate()?;
    if expires <= now(tx)? {
        return Err(Error::Precondition("credential expiry"));
    }
    let digest = token_digest(token);
    let attribution = serde_json::to_value(attribution)?;
    tx.execute("INSERT INTO alt_credentials(token_digest,actor_id,attribution,identity_source,expires_at,administrator) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(token_digest) DO NOTHING",&[&digest,&actor,&attribution,&mode(source),&integer(expires)?,&administrator])?;
    let row=tx.query_one("SELECT actor_id,attribution,identity_source,expires_at,administrator,revoked FROM alt_credentials WHERE token_digest=$1 FOR SHARE",&[&digest])?;
    if row.get::<_, String>(0) != actor
        || row.get::<_, serde_json::Value>(1) != attribution
        || row.get::<_, String>(2) != mode(source)
        || row.get::<_, i64>(3) != integer(expires)?
        || row.get::<_, bool>(4) != administrator
        || row.get::<_, bool>(5)
    {
        return Err(Error::Precondition(
            "credential secret already has different or revoked claims",
        ));
    }
    Ok(())
}
pub(crate) fn credential_row(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
) -> Result<postgres::Row> {
    let row=tx.query_opt("SELECT actor_id,attribution,identity_source,expires_at,revoked,administrator FROM alt_credentials WHERE token_digest=$1 FOR SHARE",&[&credential.digest])?.ok_or(Error::Unauthorized)?;
    if row.get::<_, String>(2) != spec.mode()
        || row.get::<_, bool>(4)
        || unsigned(row.get(3))? <= now(tx)?
    {
        return Err(Error::Unauthorized);
    }
    Ok(row)
}
pub(crate) fn require_admin(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
) -> Result<()> {
    if !credential_row(tx, spec, credential)?.get::<_, bool>(5) {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
pub(crate) fn authorize(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
    snapshot: &RepositorySnapshot,
) -> Result<Authorization> {
    let row = credential_row(tx, spec, credential)?;
    let actor: String = row.get(0);
    let grant=tx.query_opt("SELECT can_read,publish_records,grants FROM alt_grants WHERE actor_id=$1 AND repository=$2",&[&actor,&snapshot.scope.repository_instance_id])?.ok_or(Error::Unauthorized)?;
    // Administrators do not get implicit access to repository contents.
    if !grant.get::<_, bool>(0) {
        return Err(Error::Unauthorized);
    }
    Ok(Authorization {
        scope: snapshot.scope.clone(),
        runtime_generation: spec.runtime_generation,
        actor_id: actor,
        attribution: serde_json::from_value(row.get(1))?,
        expires_at: unsigned(row.get(3))?,
        revocation_generation: snapshot.revocation_generation,
        grants: serde_json::from_value(grant.get(2))?,
        publish_records: grant.get(1),
    })
}
pub(crate) fn snapshot(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    repository: &str,
) -> Result<RepositorySnapshot> {
    let row=tx.query_opt("SELECT recovery_epoch,object_format,policy_version,revocation_generation,policies,writes_enabled FROM alt_repositories WHERE id=$1 FOR UPDATE",&[&repository])?.ok_or(Error::Unauthorized)?;
    if row.get::<_, String>(1) != "sha1" {
        return Err(Error::Precondition("unsupported repository object format"));
    }
    let mut refs = std::collections::BTreeMap::new();
    for row in tx.query(
        "SELECT name,oid FROM alt_refs WHERE repository=$1 ORDER BY name",
        &[&repository],
    )? {
        let oid: String = row.get(1);
        refs.insert(
            row.get(0),
            oid.parse()
                .map_err(|_| Error::Integrity("stored reference object id"))?,
        );
    }
    Ok(RepositorySnapshot {
        scope: Scope {
            installation_id: spec.installation_id.clone(),
            repository_instance_id: repository.into(),
            recovery_epoch: unsigned(row.get(0))?,
        },
        runtime_generation: spec.runtime_generation,
        revocation_generation: unsigned(row.get(3))?,
        policy_version: unsigned(row.get(2))?,
        object_format: ObjectFormat::Sha1,
        refs,
        policies: serde_json::from_value(row.get(4))?,
        approved_publications: BTreeSet::new(),
        writes_enabled: row.get(5),
    })
}
