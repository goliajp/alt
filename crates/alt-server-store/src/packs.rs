//! Git objects as verified pack segments: one database row per segment, refs and the
//! segment published in one transaction, reads without any per-object row.

use crate::{
    Credential, Error, Result, Store,
    store::{authorize, credential_row, instance, integer, now, snapshot, unsigned},
};
use alt_git_codec::ObjectId;
use alt_pack_segment::{Segment, SegmentSet};
use alt_remote_protocol::{Publication, Receipt, ReceiptBody, RequestState, Scope};
use alt_repository_service::{Graph, validate_publication};
use ed25519_dalek::Signer;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
};

const VIEW: &str = "SELECT
 (SELECT row_to_json(i) FROM (SELECT installation_id,workspace_id,deployment_mode,runtime_generation,signing_key_id FROM alt_instance WHERE singleton) i),
 (SELECT row_to_json(c) FROM (SELECT actor_id,attribution,identity_source,expires_at,revoked FROM alt_credentials WHERE token_digest=$1) c),
 EXISTS(SELECT 1 FROM alt_delegations WHERE token_digest=$1),
 (SELECT row_to_json(a) FROM (SELECT disabled FROM alt_web_accounts WHERE actor_id=(SELECT actor_id FROM alt_credentials WHERE token_digest=$1)) a),
 (SELECT row_to_json(s) FROM (SELECT s.account_generation,a.generation,s.expires_at FROM alt_web_sessions s JOIN alt_web_accounts a ON a.actor_id=s.actor_id WHERE s.token_digest=$1) s),
 (SELECT row_to_json(g) FROM (SELECT can_read,publish_records,grants FROM alt_grants WHERE repository=$2 AND actor_id=(SELECT actor_id FROM alt_credentials WHERE token_digest=$1)) g),
 (SELECT row_to_json(r) FROM (SELECT recovery_epoch,object_format,default_branch,pack_unreachable,writes_enabled AND portable_freeze IS NULL AND portable_import IS NULL AS writable FROM alt_repositories WHERE id=$2) r),
 (SELECT coalesce(json_agg(json_build_array(name,oid) ORDER BY name),'[]'::json) FROM alt_refs WHERE repository=$2),
 (SELECT coalesce(json_agg(json_build_array(segment_id,inline IS NOT NULL) ORDER BY sequence),'[]'::json) FROM alt_pack_segments WHERE repository=$2 AND state='live'),
 floor(extract(epoch FROM clock_timestamp())*1000)::bigint";

/// segments up to this size are stored with their row instead of being flushed to disk
pub const INLINE_SEGMENT_BYTES: usize = 65536;

/// segments are immutable, so one mapping per file serves every request
static OPEN: LazyLock<Mutex<HashMap<PathBuf, Arc<Segment>>>> = LazyLock::new(Default::default);

pub struct PackView {
    pub scope: Scope,
    pub refs: BTreeMap<String, ObjectId>,
    pub default_branch: String,
    pub segments: SegmentSet,
    /// some stored objects may be unreachable from the refs
    pub unreachable: bool,
    pub actor_id: String,
    pub attribution: alt_remote_protocol::Attribution,
    /// writes are enabled and the caller holds some ref-changing grant
    pub writable: bool,
}

pub struct NewSegment<'a> {
    pub id: &'a str,
    pub objects: u64,
    pub bytes: u64,
    pub flags: u32,
    /// the whole segment file when it is small enough to be stored with its row
    pub inline: Option<&'a [u8]>,
}

impl Store {
    /// directory holding one repository's segments, created on first use
    pub fn pack_directory(&self, repository: &str) -> Result<PathBuf> {
        let packs = self.bytes().root().join("packs");
        let path = packs.join(&blake3::hash(repository.as_bytes()).to_hex()[..32]);
        if !path.is_dir() {
            std::fs::create_dir_all(&path)?;
            std::fs::File::open(&packs)?.sync_all()?;
            std::fs::File::open(self.bytes().root())?.sync_all()?;
        }
        Ok(path)
    }

    pub fn pack_path(&self, repository: &str, segment: &str) -> Result<PathBuf> {
        if segment.len() != 32 || !segment.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Integrity("segment id"));
        }
        Ok(self.pack_directory(repository)?.join(format!("{segment}.seg")))
    }

    /// makes a segment this process just wrote available to later requests
    pub fn adopt_segment(&self, repository: &str, id: &str, segment: Arc<Segment>) -> Result<()> {
        OPEN.lock().unwrap().insert(self.pack_path(repository, id)?, segment);
        Ok(())
    }

    /// `ids` pairs each live segment with whether its bytes are still held by its row
    fn open_segments(&mut self, repository: &str, ids: &[(String, bool)]) -> Result<SegmentSet> {
        let mut segments = Vec::with_capacity(ids.len());
        for (id, inline) in ids {
            let path = self.pack_path(repository, id)?;
            let cached = OPEN.lock().unwrap().get(&path).cloned();
            let segment = match cached {
                Some(segment) => segment,
                None => {
                    if *inline {
                        // the file of an unsettled segment was never flushed; the row is the truth
                        let bytes: Vec<u8> = self
                            .client
                            .query_one("SELECT inline FROM alt_pack_segments WHERE repository=$1 AND segment_id=$2", &[&repository, id])?
                            .get::<_, Option<Vec<u8>>>(0)
                            .ok_or(Error::Integrity("pack segment bytes"))?;
                        let staging = path.with_extension("restore");
                        std::fs::write(&staging, &bytes)?;
                        std::fs::rename(&staging, &path)?;
                    }
                    let segment = Arc::new(Segment::open(&path).map_err(|_| Error::Integrity("pack segment unavailable"))?);
                    OPEN.lock().unwrap().insert(path, segment.clone());
                    segment
                }
            };
            segments.push(segment);
        }
        Ok(SegmentSet::new(segments))
    }

    /// authenticates a token and opens the repository view in the same transaction
    pub fn git_open(&mut self, token: &str, repository: &str) -> Result<(Credential, PackView)> {
        if token.len() != 69 || !token.starts_with("alt1_") {
            return Err(Error::Unauthorized);
        }
        let credential = Credential { digest: crate::store::token_digest(token) };
        let view = self.pack_view(&credential, repository)?;
        Ok((credential, view))
    }

    /// refs and live segments as of one instant, for a caller allowed to read the repository
    pub fn pack_view(&mut self, credential: &Credential, repository: &str) -> Result<PackView> {
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
    fn pack_view_statement(&mut self, credential: &Credential, repository: &str) -> Result<Option<PackView>> {
        use serde_json::Value;
        let row = self.client.query_one(VIEW, &[&credential.digest, &repository])?;
        let field = |index: usize| row.get::<_, Option<Value>>(index);
        let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let number = |value: &Value, key: &str| value.get(key).and_then(Value::as_i64);
        let flag = |value: &Value, key: &str| value.get(key).and_then(Value::as_bool);
        let instance = field(0).ok_or(Error::Precondition("instance not initialized"))?;
        if text(&instance, "installation_id").as_deref() != Some(self.spec.installation_id.as_str())
            || text(&instance, "workspace_id").as_deref() != Some(self.spec.workspace_id.as_str())
            || text(&instance, "deployment_mode").as_deref() != Some("self_hosted")
            || number(&instance, "runtime_generation") != Some(integer(self.spec.runtime_generation)?)
            || text(&instance, "signing_key_id").as_deref() != Some(blake3::hash(self.signing_key.verifying_key().as_bytes()).to_hex().as_str())
        {
            return Err(Error::Precondition("instance identity, mode, generation or signing key"));
        }
        if row.get::<_, bool>(2) {
            return Ok(None);
        }
        let time: i64 = row.get(9);
        let credential_row = field(1).ok_or(Error::Unauthorized)?;
        let attribution = credential_row.get("attribution").cloned().unwrap_or(Value::Null);
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
        let grants: Vec<alt_repository_service::Grant> = serde_json::from_value(grant.get("grants").cloned().unwrap_or(Value::Null))?;
        let mut refs = BTreeMap::new();
        for pair in field(7).and_then(|value| value.as_array().cloned()).unwrap_or_default() {
            let (Some(name), Some(oid)) = (pair.get(0).and_then(Value::as_str), pair.get(1).and_then(Value::as_str)) else {
                return Err(Error::Integrity("stored reference"));
            };
            refs.insert(name.to_owned(), oid.parse().map_err(|_| Error::Integrity("stored reference object id"))?);
        }
        let ids: Vec<(String, bool)> = serde_json::from_value(field(8).unwrap_or(Value::Null))?;
        Ok(Some(PackView {
            scope: Scope {
                installation_id: self.spec.installation_id.clone(),
                repository_instance_id: repository.into(),
                recovery_epoch: unsigned(number(&repository_row, "recovery_epoch").ok_or(Error::Integrity("repository epoch"))?)?,
            },
            refs,
            default_branch: text(&repository_row, "default_branch").ok_or(Error::Integrity("default branch"))?,
            unreachable: flag(&repository_row, "pack_unreachable") == Some(true),
            writable: flag(&repository_row, "writable") == Some(true) && grants.iter().any(|grant| grant.create || grant.update || grant.delete),
            actor_id: text(&credential_row, "actor_id").ok_or(Error::Integrity("credential actor"))?,
            attribution: serde_json::from_value(attribution)?,
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
        let row = tx.query_one("SELECT default_branch,pack_unreachable FROM alt_repositories WHERE id=$1", &[&repository])?;
        let ids: Vec<(String, bool)> = tx
            .query("SELECT segment_id,inline IS NOT NULL FROM alt_pack_segments WHERE repository=$1 AND state='live' ORDER BY sequence", &[&repository])?
            .iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect();
        tx.commit()?;
        Ok(PackView {
            scope: current.scope,
            refs: current.refs,
            default_branch: row.get(0),
            unreachable: row.get(1),
            writable: current.writes_enabled && auth.grants.iter().any(|grant| grant.create || grant.update || grant.delete),
            actor_id: auth.actor_id,
            attribution: auth.attribution,
            segments: self.open_segments(repository, &ids)?,
        })
    }

    /// Publishes ref updates, and the segment that carries their new objects, in one
    /// transaction. The segment file must already be durable under its final name.
    pub fn pack_publish(
        &mut self,
        credential: &Credential,
        publication: &Publication,
        graph: &impl Graph,
        segment: Option<NewSegment<'_>>,
    ) -> Result<Receipt> {
        let repository = &publication.key.scope.repository_instance_id;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let time = now(&mut tx)?;
        let validated = validate_publication(publication, &current, &auth, graph, self.limits, time)?;
        let mut names = Vec::new();
        let mut oids = Vec::new();
        let mut deleted = Vec::new();
        for update in &validated.updates {
            match update.new {
                Some(oid) => {
                    names.push(update.name.as_str());
                    oids.push(oid.to_string());
                }
                None => deleted.push(update.name.as_str()),
            }
        }
        if !names.is_empty() {
            tx.execute(
                "INSERT INTO alt_refs(repository,name,oid) SELECT $1,name,oid FROM unnest($2::text[],$3::text[]) AS refs(name,oid) ON CONFLICT(repository,name) DO UPDATE SET oid=excluded.oid",
                &[repository, &names, &oids],
            )?;
        }
        if !deleted.is_empty() {
            tx.execute("DELETE FROM alt_refs WHERE repository=$1 AND name=ANY($2)", &[repository, &deleted])?;
        }
        let unreachable = publication.refs.iter().any(|update| update.force || update.new.is_none());
        let sequence = unsigned(
            tx.query_one(
                "UPDATE alt_repositories SET transaction_sequence=transaction_sequence+1,pack_unreachable=pack_unreachable OR $2 WHERE id=$1 RETURNING transaction_sequence",
                &[repository, &unreachable],
            )?
            .get(0),
        )?;
        if let Some(segment) = &segment {
            if tx.execute(
                "UPDATE alt_repositories SET stored_bytes=stored_bytes+$2 WHERE id=$1 AND $2<=quota_bytes-stored_bytes-reserved_bytes-link_bytes",
                &[repository, &integer(segment.bytes)?],
            )? != 1
            {
                return Err(Error::Limit("repository quota"));
            }
            tx.execute(
                "INSERT INTO alt_pack_segments(repository,segment_id,sequence,objects,bytes,flags,state,inline) VALUES($1,$2,$3,$4,$5,$6,'live',$7)",
                &[repository, &segment.id, &integer(sequence)?, &integer(segment.objects)?, &integer(segment.bytes)?, &(segment.flags as i32), &segment.inline],
            )?;
        }
        let body = ReceiptBody {
            publication: publication.clone(),
            request_digest: validated.digest.clone(),
            transaction_sequence: sequence,
            policy_version: validated.policy_version,
            committed_at_unix_ms: now(&mut tx)?,
        };
        let signature = self.signing_key.sign(&body.signing_bytes(self.limits)?);
        let receipt = Receipt {
            body,
            signing_key_id: blake3::hash(self.signing_key.verifying_key().as_bytes()).to_hex().to_string(),
            signature: signature.to_bytes().iter().map(|byte| format!("{byte:02x}")).collect(),
        };
        let event = serde_json::json!({"kind":"publication_committed","receipt":receipt});
        tx.execute(
            "INSERT INTO alt_audit(repository,sequence,actor_id,event) VALUES($1,$2,$3,$4)",
            &[repository, &integer(sequence)?, &auth.actor_id, &event],
        )?;
        tx.execute("INSERT INTO alt_outbox(repository,sequence,event) VALUES($1,$2,$3)", &[repository, &integer(sequence)?, &event])?;
        let result = RequestState::Committed { receipt: Box::new(receipt.clone()) };
        tx.execute(
            "INSERT INTO alt_requests(repository,epoch,actor_id,request_id,digest,publication,manifest,state,result,expires_at,reserved_bytes) VALUES($1,$2,$3,$4,$5,$6,$7,'committed',$8,0,0)",
            &[
                repository,
                &integer(publication.key.scope.recovery_epoch)?,
                &publication.key.actor_id,
                &publication.key.request_id,
                &validated.digest,
                &serde_json::to_value(publication)?,
                &serde_json::json!({"pack_segment": segment.as_ref().map(|segment| segment.id)}),
                &serde_json::to_value(&result)?,
            ],
        )?;
        // lock waits can outlive a lease checked on entry
        credential_row(&mut tx, &self.spec, credential)?;
        crate::cloud_quota::check_publication(&mut tx, &self.spec)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(receipt)
    }
}
