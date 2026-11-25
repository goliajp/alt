use crate::Jsonb;
use crate::database::Transaction;
pub(crate) use crate::repository_authority::{RepositoryAuthority, repository_authority};
use crate::{ByteStore, Error, Result};
use alt_crypto::ed25519::SigningKey;
use alt_pg::Client;
use alt_remote_protocol::{Attribution, DeploymentMode, Limits, ObjectFormat, Scope};
use alt_repository_service::{Authorization, Grant, RefPolicy, RepositorySnapshot};
use std::collections::BTreeSet;
mod authority;
pub(crate) use authority::{authorize, authorize_scope, credential_row, require_admin, snapshot};
mod tokens;
use tokens::insert_token;
pub(crate) use tokens::{insert_known_token, token_digest, validate_token};
mod instance;
use instance::mode;
pub(crate) use instance::{instance, instance_unlocked};

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
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

impl Credential {
    /// Stable opaque credential identity for explicit integration installation.
    /// This digest cannot be used as a bearer or construct an authenticated handle.
    pub fn identifier(&self) -> &str {
        &self.digest
    }
}

pub struct Store {
    pub(crate) client: crate::database::Connection,
    pub(crate) bytes: ByteStore,
    pub(crate) spec: InstanceSpec,
    pub(crate) signing_key: SigningKey,
    pub(crate) limits: Limits,
    /// set while a request that may stop being waited for is served, for the
    /// walks it asks of the refs
    pub(crate) waited: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl Store {
    /// Explicit first initialization, never invoked as a fallback from open.
    /// The caller supplies a dedicated database connection and persisted key.
    pub fn initialize(
        client: Client,
        bytes: ByteStore,
        spec: InstanceSpec,
        signing_key: SigningKey,
        limits: Limits,
    ) -> Result<Self> {
        if spec.deployment_mode != DeploymentMode::SelfHosted {
            return Err(Error::Precondition(
                "Cloud requires explicit platform binding",
            ));
        }
        Self::initialize_bound(client, bytes, spec, signing_key, limits, None)
    }
    pub(crate) fn initialize_bound(
        mut client: Client,
        bytes: ByteStore,
        spec: InstanceSpec,
        signing_key: SigningKey,
        limits: Limits,
        cloud: Option<(&golia_platform_contract::Audience, &str, Option<&str>)>,
    ) -> Result<Self> {
        spec.validate()?;
        limits.validate()?;
        let mut tx = Transaction::uncached(client.transaction()?);
        tx.query_one("SELECT pg_advisory_xact_lock(715071909111)", &[])?;
        tx.batch_execute("CREATE TABLE IF NOT EXISTS alt_schema(version integer PRIMARY KEY, checksum text NOT NULL)")?;
        if !tx.query("SELECT version FROM alt_schema", &[])?.is_empty() {
            return Err(Error::Precondition(
                "instance already initialized; use open",
            ));
        }
        crate::schema::create(tx.raw_mut())?;
        let key_id = alt_hash::blake3::hash(signing_key.verifying_key().as_bytes())
            .to_hex()
            .to_string();
        tx.execute("INSERT INTO alt_instance(singleton,installation_id,workspace_id,deployment_mode,runtime_generation,signing_key_id) VALUES(true,$1,$2,$3,$4,$5)", &[&spec.installation_id,&spec.workspace_id,&spec.mode(),&integer(spec.runtime_generation)?,&key_id])?;
        if let Some((audience, digest, provision_digest)) = cloud {
            crate::cloud::initialize_binding(&mut tx, &spec, audience, digest, provision_digest)?;
        }
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
        crate::schema::check_current(&mut client)?;
        let mut tx = Transaction::uncached(client.transaction()?);
        instance(&mut tx, &spec, &signing_key)?;
        tx.commit()?;
        Ok(Self {
            client: crate::database::Connection::new(client),
            bytes,
            spec,
            signing_key,
            limits,
            waited: None,
        })
    }
    /// What the walks of the refs this store is asked for look at to end
    /// early, until it is told none again: a request that is waited for no
    /// longer.
    pub fn waited_for(&mut self, gone: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) {
        self.waited = gone;
    }
    pub fn spec(&self) -> &InstanceSpec {
        &self.spec
    }
    /// Revalidate a retained connection before reuse. No schema, instance or
    /// Cloud authority decision is cached; operations still check credentials
    /// and repository state in their own transactions.
    /// the database connection ended; the store cannot serve another request
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }
    pub fn check_connection(&mut self) -> Result<()> {
        let mut tx = self.client.transaction()?;
        // only row locks are taken; nothing here needs a synchronous flush
        tx.execute("SET LOCAL synchronous_commit=off", &[])?;
        let rows = tx.query(
            "SELECT version,checksum FROM alt_schema ORDER BY version",
            &[],
        )?;
        crate::schema::check_rows(&rows, crate::SCHEMA_VERSION as usize)?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        tx.commit()?;
        Ok(())
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
        crate::browser::audit_details(
            &mut tx,
            actor,
            "instance.bootstrap",
            actor,
            &Jsonb(
                alt_json::json!({"credential_id":token_digest(token),"expires_at_unix_ms":expires_at_unix_ms.to_string()}),
            ),
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
        // only row locks are taken; nothing here needs a synchronous flush
        tx.execute("SET LOCAL synchronous_commit=off", &[])?;
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
        if self.spec.deployment_mode != DeploymentMode::SelfHosted
            || attribution.kind == alt_remote_protocol::ActorKind::Agent
        {
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
        crate::browser::audit_admin(
            &mut tx,
            &self.spec,
            admin,
            "credential.issue",
            actor,
            Jsonb(
                alt_json::json!({"credential_id":token_digest(&token),"attribution":attribution,"expires_at_unix_ms":expires_at_unix_ms.to_string(),"administrator":administrator}),
            ),
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
        if self.spec.deployment_mode != DeploymentMode::SelfHosted
            || attribution.kind == alt_remote_protocol::ActorKind::Agent
        {
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
        crate::browser::audit_admin(
            &mut tx,
            &self.spec,
            admin,
            "credential.issue",
            actor,
            Jsonb(
                alt_json::json!({"credential_id":token_digest(token),"attribution":attribution,"expires_at_unix_ms":expires_at_unix_ms.to_string(),"administrator":administrator}),
            ),
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
        crate::cloud::repository_quota(&mut tx, &self.spec)?;
        tx.execute("INSERT INTO alt_repositories(id,recovery_epoch,object_format,quota_bytes) VALUES($1,1,'sha1',$2)",&[&repository,&integer(quota_bytes)?])?;
        crate::browser::audit_admin(
            &mut tx,
            &self.spec,
            admin,
            "repository.create",
            repository,
            Jsonb(alt_json::json!({"quota_bytes":quota_bytes.to_string()})),
        )?;
        tx.commit()?;
        // the first push then pays no directory creation
        self.pack_directory(repository)?;
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
        tx.execute("INSERT INTO alt_grants(actor_id,repository,can_read,publish_records,grants) VALUES($1,$2,$3,$4,$5) ON CONFLICT(actor_id,repository) DO UPDATE SET can_read=excluded.can_read,publish_records=excluded.publish_records,grants=excluded.grants",&[&actor,&repository,&can_read,&publish_records,&Jsonb::from(&grants)?])?;
        tx.execute(
            "UPDATE alt_repositories SET revocation_generation=revocation_generation+1 WHERE id=$1",
            &[&repository],
        )?;
        crate::browser::audit_admin(
            &mut tx,
            &self.spec,
            admin,
            "repository.grants",
            repository,
            Jsonb(
                alt_json::json!({"actor_id":actor,"can_read":can_read,"publish_records":publish_records,"grants":grants}),
            ),
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
            &[&repository, &Jsonb::from(&policies)?],
        )?;
        crate::browser::audit_admin(
            &mut tx,
            &self.spec,
            admin,
            "repository.policies",
            repository,
            Jsonb(alt_json::json!({"policies":policies})),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn revoke(&mut self, admin: &Credential, token: &str) -> Result<()> {
        let mut tx = self.client.transaction()?;
        // Credential revocation takes the instance barrier before credential
        // rows, including when two administrators revoke each other.
        tx.query_one(
            "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
            &[],
        )?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        let actor: String = credential_row(&mut tx, &self.spec, admin)?.get("actor_id");
        tx.execute(
            "UPDATE alt_credentials SET revoked=true,revoked_at=COALESCE(revoked_at,floor(extract(epoch FROM clock_timestamp())*1000)::bigint) WHERE token_digest=$1",
            &[&token_digest(token)],
        )?;
        crate::browser::audit_details(
            &mut tx,
            &actor,
            "credential.revoke",
            &token_digest(token),
            &Jsonb(alt_json::json!({})),
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
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
