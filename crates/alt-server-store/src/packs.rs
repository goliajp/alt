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
}

pub struct NewSegment<'a> {
    pub id: &'a str,
    pub objects: u64,
    pub bytes: u64,
    pub flags: u32,
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

    fn open_segments(&self, repository: &str, ids: &[String]) -> Result<SegmentSet> {
        let mut segments = Vec::with_capacity(ids.len());
        for id in ids {
            let path = self.pack_path(repository, id)?;
            let cached = OPEN.lock().unwrap().get(&path).cloned();
            let segment = match cached {
                Some(segment) => segment,
                None => {
                    let segment = Arc::new(Segment::open(&path).map_err(|_| Error::Integrity("pack segment unavailable"))?);
                    OPEN.lock().unwrap().insert(path, segment.clone());
                    segment
                }
            };
            segments.push(segment);
        }
        Ok(SegmentSet::new(segments))
    }

    /// refs and live segments as of one instant, for a caller allowed to read the repository
    pub fn pack_view(&mut self, credential: &Credential, repository: &str) -> Result<PackView> {
        let mut tx = self.client.transaction()?;
        // nothing durable is written here; row locks alone need no synchronous flush
        tx.batch_execute("SET LOCAL synchronous_commit=off")?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let row = tx.query_one("SELECT default_branch,pack_unreachable FROM alt_repositories WHERE id=$1", &[&repository])?;
        let ids: Vec<String> = tx
            .query("SELECT segment_id FROM alt_pack_segments WHERE repository=$1 AND state='live' ORDER BY sequence", &[&repository])?
            .iter()
            .map(|row| row.get(0))
            .collect();
        tx.commit()?;
        Ok(PackView {
            scope: current.scope,
            refs: current.refs,
            default_branch: row.get(0),
            unreachable: row.get(1),
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
                "INSERT INTO alt_pack_segments(repository,segment_id,sequence,objects,bytes,flags,state) VALUES($1,$2,$3,$4,$5,$6,'live')",
                &[repository, &segment.id, &integer(sequence)?, &integer(segment.objects)?, &integer(segment.bytes)?, &(segment.flags as i32)],
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
