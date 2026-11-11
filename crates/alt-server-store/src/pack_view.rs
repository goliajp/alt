//! The view a Git request works from: who is asking, what they may do, and the
//! repository's references and live segments as of one instant.

use crate::Jsonb;
use crate::{
    Credential, Error, Refs, Result, Store,
    store::{authorize, instance, integer, snapshot, unsigned},
};
use alt_pack_segment::SegmentSet;
use alt_remote_protocol::Scope;

const VIEW: &str = "SELECT
 (SELECT row_to_json(i) FROM (SELECT installation_id,workspace_id,deployment_mode,runtime_generation,signing_key_id FROM alt_instance WHERE singleton) i),
 (SELECT row_to_json(c) FROM (SELECT actor_id,attribution,identity_source,expires_at,revoked FROM alt_credentials WHERE token_digest=$1) c),
 EXISTS(SELECT 1 FROM alt_delegations WHERE token_digest=$1),
 (SELECT row_to_json(a) FROM (SELECT disabled FROM alt_web_accounts WHERE actor_id=(SELECT actor_id FROM alt_credentials WHERE token_digest=$1)) a),
 (SELECT row_to_json(s) FROM (SELECT s.account_generation,a.generation,s.expires_at FROM alt_web_sessions s JOIN alt_web_accounts a ON a.actor_id=s.actor_id WHERE s.token_digest=$1) s),
 (SELECT row_to_json(g) FROM (SELECT can_read,publish_records,grants FROM alt_grants WHERE repository=$2 AND actor_id=(SELECT actor_id FROM alt_credentials WHERE token_digest=$1)) g),
 (SELECT row_to_json(r) FROM (SELECT recovery_epoch,object_format,default_branch,pack_unreachable,writes_enabled AND portable_freeze IS NULL AND portable_import IS NULL AS writable FROM alt_repositories WHERE id=$2) r),
 (SELECT string_agg(oid||' '||name,E'\\n') FROM alt_refs WHERE repository=$2),
 (SELECT coalesce(json_agg(json_build_array(segment_id,inline IS NOT NULL) ORDER BY sequence),'[]'::json) FROM alt_pack_segments WHERE repository=$2 AND state='live'),
 floor(extract(epoch FROM clock_timestamp())*1000)::bigint";

pub struct PackView {
    pub scope: Scope,
    pub refs: Refs,
    pub default_branch: String,
    pub segments: SegmentSet,
    /// some stored objects may be unreachable from the refs
    pub unreachable: bool,
    pub actor_id: String,
    pub attribution: alt_remote_protocol::Attribution,
    /// writes are enabled and the caller holds some ref-changing grant
    pub writable: bool,
}

impl Store {
    /// authenticates a token and opens the repository view in the same transaction
    pub fn git_open(&mut self, token: &str, repository: &str) -> Result<(Credential, PackView)> {
        if token.len() != 69 || !token.starts_with("alt1_") {
            return Err(Error::Unauthorized);
        }
        let credential = Credential {
            digest: crate::store::token_digest(token),
        };
        let view = self.pack_view(&credential, repository)?;
        Ok((credential, view))
    }

    /// refs and live segments as of one instant, for a caller allowed to read the repository
    pub fn pack_view(&mut self, credential: &Credential, repository: &str) -> Result<PackView> {
        match self.pack_view_once(credential, repository) {
            // a merge retired segments between the list being read and the files
            // being opened, and their time on disk ran out: the new list has none of them
            Err(Error::Integrity("pack segment unavailable")) => {
                self.pack_view_once(credential, repository)
            }
            view => view,
        }
    }

    fn pack_view_once(&mut self, credential: &Credential, repository: &str) -> Result<PackView> {
        if self.spec.deployment_mode == alt_remote_protocol::DeploymentMode::SelfHosted
            && let Some(view) = self.pack_view_statement(credential, repository)?
        {
            return Ok(view);
        }
        self.pack_view_locked(credential, repository)
    }

    /// The same decision as `pack_view_locked` from one statement and therefore one
    /// snapshot, without row locks. Delegated credentials return None and take the
    /// transactional path, which owns the delegation rules.
    fn pack_view_statement(
        &mut self,
        credential: &Credential,
        repository: &str,
    ) -> Result<Option<PackView>> {
        use alt_json::Value;
        let row = self
            .client
            .query_one(VIEW, &[&credential.digest, &repository])?;
        let field = |index: usize| row.get::<_, Option<Jsonb>>(index).map(|json| json.0);
        let text =
            |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let number = |value: &Value, key: &str| value.get(key).and_then(Value::as_i64);
        let flag = |value: &Value, key: &str| value.get(key).and_then(Value::as_bool);
        let instance = field(0).ok_or(Error::Precondition("instance not initialized"))?;
        if text(&instance, "installation_id").as_deref() != Some(self.spec.installation_id.as_str())
            || text(&instance, "workspace_id").as_deref() != Some(self.spec.workspace_id.as_str())
            || text(&instance, "deployment_mode").as_deref() != Some("self_hosted")
            || number(&instance, "runtime_generation")
                != Some(integer(self.spec.runtime_generation)?)
            || text(&instance, "signing_key_id").as_deref()
                != Some(
                    alt_hash::blake3::hash(self.signing_key.verifying_key().as_bytes())
                        .to_hex()
                        .as_str(),
                )
        {
            return Err(Error::Precondition(
                "instance identity, mode, generation or signing key",
            ));
        }
        if row.get::<_, bool>(2) {
            return Ok(None);
        }
        let time: i64 = row.get(9);
        let credential_row = field(1).ok_or(Error::Unauthorized)?;
        let attribution = credential_row
            .get("attribution")
            .cloned()
            .unwrap_or(Value::Null);
        if text(&credential_row, "identity_source").as_deref() != Some("self_hosted")
            || flag(&credential_row, "revoked") != Some(false)
            || number(&credential_row, "expires_at").is_none_or(|expires| expires <= time)
            || attribution.get("kind").and_then(Value::as_str) == Some("agent")
        {
            return Err(Error::Unauthorized);
        }
        if field(3).is_some_and(|account| flag(&account, "disabled") != Some(false)) {
            return Err(Error::Unauthorized);
        }
        if let Some(session) = field(4)
            && (number(&session, "account_generation") != number(&session, "generation")
                || number(&session, "expires_at").is_none_or(|expires| expires <= time))
        {
            return Err(Error::Unauthorized);
        }
        crate::telemetry::authenticated_as(&attribution);
        let repository_row = field(6).ok_or(Error::Unauthorized)?;
        if text(&repository_row, "object_format").as_deref() != Some("sha1") {
            return Err(Error::Precondition("unsupported repository object format"));
        }
        let grant = field(5).ok_or(Error::Unauthorized)?;
        // administrators do not get implicit access to repository contents
        if flag(&grant, "can_read") != Some(true) {
            return Err(Error::Unauthorized);
        }
        let grants: Vec<alt_repository_service::Grant> =
            alt_json::from_value(grant.get("grants").cloned().unwrap_or(Value::Null))?;
        let refs = Refs::parse(row.get::<_, Option<&str>>(7))?;
        let ids: Vec<(String, bool)> = alt_json::from_value(field(8).unwrap_or(Value::Null))?;
        Ok(Some(PackView {
            scope: Scope {
                installation_id: self.spec.installation_id.clone(),
                repository_instance_id: repository.into(),
                recovery_epoch: unsigned(
                    number(&repository_row, "recovery_epoch")
                        .ok_or(Error::Integrity("repository epoch"))?,
                )?,
            },
            refs,
            default_branch: text(&repository_row, "default_branch")
                .ok_or(Error::Integrity("default branch"))?,
            unreachable: flag(&repository_row, "pack_unreachable") == Some(true),
            writable: flag(&repository_row, "writable") == Some(true)
                && grants
                    .iter()
                    .any(|grant| grant.create || grant.update || grant.delete),
            actor_id: text(&credential_row, "actor_id")
                .ok_or(Error::Integrity("credential actor"))?,
            attribution: alt_json::from_value(attribution)?,
            segments: self.open_segments(repository, &ids)?,
        }))
    }

    fn pack_view_locked(&mut self, credential: &Credential, repository: &str) -> Result<PackView> {
        let mut tx = self.client.transaction()?;
        // nothing durable is written here; row locks alone need no synchronous flush
        tx.execute("SET LOCAL synchronous_commit=off", &[])?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let row = tx.query_one(
            "SELECT default_branch,pack_unreachable FROM alt_repositories WHERE id=$1",
            &[&repository],
        )?;
        let ids = Self::live_segment_ids(&mut tx, repository)?;
        tx.commit()?;
        Ok(PackView {
            scope: current.scope,
            refs: Refs::from(&current.refs),
            default_branch: row.get(0),
            unreachable: row.get(1),
            writable: current.writes_enabled
                && auth
                    .grants
                    .iter()
                    .any(|grant| grant.create || grant.update || grant.delete),
            actor_id: auth.actor_id,
            attribution: auth.attribution,
            segments: self.open_segments(repository, &ids)?,
        })
    }
}
