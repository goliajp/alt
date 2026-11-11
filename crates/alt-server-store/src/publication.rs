mod acquire;
mod fence;
mod pack;
mod refs;
mod source;

pub(crate) use fence::{check_graph_pins, finish, worker};

use crate::database::Transaction;
use crate::requests::{active, check_key, pin_many, request_row, upload_pin};
use crate::store::{authorize, instance, integer, now, snapshot, unsigned};
use crate::{ByteStore, Credential, Error, Result, Store};
use alt_git_codec::{ObjectId, ObjectKind};
use alt_remote_protocol::{Manifest, Publication, Receipt, ReceiptBody, RequestKey, RequestState};
use alt_repository_service::{ObjectReader, ObjectSource, VerifiedGraph, validate_publication};
use std::collections::{BTreeMap, BTreeSet};

/// A leased worker snapshot. The fence is rechecked for every transition; a
/// stale worker can leave orphan bytes but cannot publish or change a result.
pub struct PublicationWork {
    pub(crate) key: RequestKey,
    pub(crate) fence: u64,
    pub(crate) publication: Publication,
    pub(crate) manifest: Manifest,
    pub(crate) source: PinnedSource,
}
impl PublicationWork {
    pub fn publication(&self) -> &Publication {
        &self.publication
    }
    pub fn source(&self) -> &impl ObjectSource {
        &self.source
    }
    /// Finish physical index verification before exposing a buffered review.
    /// This grants no authority; callers still need current store checks.
    pub fn check_read_integrity(&self) -> Result<()> {
        self.source.bytes.finish()
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn artifacts(&self) -> &impl alt_task_service::ArtifactSource {
        &self.source
    }
    pub fn roots(&self) -> Result<Vec<ObjectId>> {
        self.publication
            .refs
            .iter()
            .flat_map(|update| [&update.expected, &update.new])
            .flatten()
            .map(|oid| {
                oid.parse()
                    .map_err(|_| Error::Integrity("publication object id"))
            })
            .collect()
    }
}
/// the live segments were not the ones the request's objects were packed against
const SEGMENTS_MOVED: &str = "repository segments changed during publication; publish again";
/// what became of packing the request's Git objects
enum Packed {
    Ready(Option<crate::packs::Materialized>),
    /// a selected object could not be read back
    Unavailable,
    /// the selected objects are not a closed set
    Invalid,
}
pub(crate) struct PinnedSource {
    bytes: crate::bytes::ReadSession,
    pub(crate) objects: BTreeMap<ObjectId, (ObjectKind, String, u64)>,
    artifacts: BTreeMap<String, u64>,
    locations: crate::segments::Locations,
    /// Git objects published earlier, whichever protocol brought them in
    pub(crate) segments: alt_pack_segment::SegmentSet,
}
impl Store {
    pub fn renew_publication(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        lease_ms: u64,
    ) -> Result<()> {
        if lease_ms == 0 || lease_ms > 300_000 {
            return Err(Error::Limit("worker lease"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(&work.key, &current.scope, &auth.actor_id)?;
        let row = worker(&mut tx, work)?;
        crate::requests::check_request_attribution(&row, &auth)?;
        let expiry = now(&mut tx)?
            .checked_add(lease_ms)
            .ok_or(Error::Limit("worker lease"))?
            .min(unsigned(row.get("expires_at"))?);
        tx.execute("UPDATE alt_requests SET worker_lease_until=$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[&work.key.scope.repository_instance_id, &integer(work.key.scope.recovery_epoch)?, &work.key.actor_id, &work.key.request_id, &integer(expiry)?])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(())
    }
    /// Persist a validation failure without erasing its original request ID.
    /// Only the current fenced worker can decide this terminal result.
    pub fn reject_publication(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        code: alt_remote_protocol::ErrorCode,
        message: &str,
    ) -> Result<RequestState> {
        if message.is_empty() || message.len() > 4096 {
            return Err(Error::Limit("rejection message"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(&work.key, &current.scope, &auth.actor_id)?;
        let row = worker(&mut tx, work)?;
        crate::requests::check_request_attribution(&row, &auth)?;
        let state = RequestState::Rejected {
            digest: row.get("digest"),
            failure: alt_remote_protocol::Failure {
                code,
                message: message.into(),
                request: Some(work.key.clone()),
                retry: alt_remote_protocol::Retry::NewIntent,
            },
        };
        finish(
            &mut tx,
            &work.key,
            "rejected",
            &state,
            unsigned(row.get("reserved_bytes"))?,
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(state)
    }
    pub fn mark_ready(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
    ) -> Result<()> {
        self.mark_ready_checked(credential, work, graph, None)
    }
    pub fn mark_ready_with_tasks(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: &alt_task_service::VerifiedTaskGraph,
    ) -> Result<()> {
        self.mark_ready_checked(credential, work, graph, Some(tasks))
    }
    fn mark_ready_checked(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: Option<&alt_task_service::VerifiedTaskGraph>,
    ) -> Result<()> {
        crate::tasks::check_selection(work, tasks)?;
        // Reusing published bytes skips upload registration, but it must not
        // skip this request's chunk claims. Do the bounded I/O before SQL locks.
        for entry in &work.manifest.entries {
            ByteStore::verify_chunk_reader(work.source.entry_reader(entry)?, entry)?;
        }
        work.source.bytes.finish()?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(&work.key, &current.scope, &auth.actor_id)?;
        let row = worker(&mut tx, work)?;
        crate::requests::check_request_attribution(&row, &auth)?;
        for root in work.roots()? {
            if !graph.holds(root) {
                return Err(alt_repository_service::Error::Missing(root).into());
            }
        }
        // Graph validation must have used this worker's authorized object set.
        check_graph_pins(&mut tx, work, graph)?;
        crate::tasks::check_pins(&mut tx, work, graph, tasks)?;
        let state = RequestState::Ready {
            digest: row.get("digest"),
        };
        tx.execute("UPDATE alt_requests SET state='ready',result=$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4",&[&work.key.scope.repository_instance_id,&integer(work.key.scope.recovery_epoch)?,&work.key.actor_id,&work.key.request_id,&serde_json::to_value(&state)?])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(())
    }
    /// Byte graph validation happens outside this short transaction. The
    /// transaction rechecks all mutable authority, CAS, policy and pin fences.
    pub fn publish(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
    ) -> Result<Receipt> {
        self.publish_checked(credential, work, graph, None)
    }
    pub fn publish_with_tasks(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: &alt_task_service::VerifiedTaskGraph,
    ) -> Result<Receipt> {
        self.publish_checked(credential, work, graph, Some(tasks))
    }
    fn publish_checked(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: Option<&alt_task_service::VerifiedTaskGraph>,
    ) -> Result<Receipt> {
        crate::telemetry::request(&work.key);
        crate::telemetry::observe(alt_server_telemetry::Phase::Commit, || {
            let receipt = self.publish_checked_inner(credential, work, graph, tasks)?;
            crate::telemetry::committed(&receipt);
            Ok(receipt)
        })
    }
    fn publish_checked_inner(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: Option<&alt_task_service::VerifiedTaskGraph>,
    ) -> Result<Receipt> {
        let repository = &work.key.scope.repository_instance_id;
        // What has to be packed depends on the segments that are live when the
        // transaction runs: another publication can add one, with some of the same
        // objects, after this request was leased. Pack against the current list, and
        // again if the list has moved by the time the repository is locked.
        for _ in 0..8 {
            // Recheck before taking SQL locks. A durable committed receipt remains
            // replayable even if its formerly selected byte files are unavailable.
            let read_integrity = work.check_read_integrity();
            let (ids, live) = self.live_segments(repository)?;
            // a failure here must not hide the receipt of an already committed request
            let packed = match self.materialize_publication(work, graph, &live) {
                Ok(segment) => Packed::Ready(segment),
                Err(Error::Integrity(crate::pack_native::INVALID_SEGMENT)) => Packed::Invalid,
                Err(_) => Packed::Unavailable,
            };
            let result = self.publish_transaction(
                credential,
                work,
                graph,
                tasks,
                read_integrity,
                (&packed, &ids),
            );
            match (&result, packed) {
                (Ok(_), Packed::Ready(Some(segment))) => {
                    self.adopt_segment(repository, &segment.id, segment.segment)?
                }
                // the transaction may have committed: the sweep removes the file if it did not
                (Err(Error::OutcomeUnknown), _) => {}
                (Err(_), Packed::Ready(Some(segment))) => {
                    // nothing references an unpublished segment
                    let _ = std::fs::remove_file(&segment.path);
                }
                _ => {}
            }
            if !matches!(result, Err(Error::Precondition(SEGMENTS_MOVED))) {
                return result;
            }
        }
        Err(Error::Precondition(SEGMENTS_MOVED))
    }
    fn publish_transaction(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: Option<&alt_task_service::VerifiedTaskGraph>,
        read_integrity: Result<()>,
        // what became of packing, and the live segments it was packed against
        (packed, packed_against): (&Packed, &[(String, bool)]),
    ) -> Result<Receipt> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let mut current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(&work.key, &current.scope, &auth.actor_id)?;
        let row = request_row(&mut tx, &work.key)?
            .ok_or(Error::Precondition("request not registered"))?;
        crate::requests::check_request_attribution(&row, &auth)?;
        if row.get::<_, String>("state") == "committed" {
            let state: RequestState = serde_json::from_value(row.get("result"))?;
            if let RequestState::Committed { receipt } = state {
                return Ok(*receipt);
            }
            return Err(Error::Integrity("committed request result"));
        }
        if Self::live_segment_ids(&mut tx, &work.key.scope.repository_instance_id)?
            != packed_against
        {
            return Err(Error::Precondition(SEGMENTS_MOVED));
        }
        read_integrity?;
        if matches!(packed, Packed::Unavailable) {
            return Err(Error::Integrity("selected Git object unavailable"));
        }
        let _ = worker(&mut tx, work)?;
        if row.get::<_, String>("state") != "ready" {
            return Err(Error::Precondition("request not ready"));
        }
        let reject = |tx: &mut Transaction<'_>, code, message: String| -> Result<()> {
            use alt_remote_protocol::{Failure, Retry};
            let result = RequestState::Rejected {
                digest: row.get("digest"),
                failure: Failure {
                    code,
                    message,
                    request: Some(work.key.clone()),
                    retry: Retry::NewIntent,
                },
            };
            finish(
                tx,
                &work.key,
                "rejected",
                &result,
                unsigned(row.get("reserved_bytes"))?,
            )
        };
        let materialized = match packed {
            Packed::Ready(segment) => segment.as_ref(),
            // a selected object refers to one neither selected nor published
            _ => {
                reject(
                    &mut tx,
                    alt_remote_protocol::ErrorCode::IntegrityError,
                    crate::pack_native::INVALID_SEGMENT.into(),
                )?;
                tx.commit().map_err(|_| Error::OutcomeUnknown)?;
                return Err(Error::Integrity(crate::pack_native::INVALID_SEGMENT));
            }
        };
        crate::tasks::check_selection(work, tasks)?;
        crate::tasks::check_pins(&mut tx, work, graph, tasks)?;
        crate::tasks::approve_current(&mut tx, &self.spec, work, tasks, &mut current, self.limits)?;
        // verification stopped at what the segments already hold; ancestry and path
        // checks walk on there
        let published = crate::packs::SegmentGraph {
            set: &work.source.segments,
            limits: self.limits.into(),
        };
        let layered = alt_repository_service::Layered {
            verified: graph,
            settled: &published,
        };
        let validated = match validate_publication(
            &work.publication,
            &current,
            &auth,
            &layered,
            self.limits,
            now(&mut tx)?,
        ) {
            Ok(validated) => validated,
            Err(error) => {
                use alt_remote_protocol::ErrorCode;
                let code = match &error {
                    alt_repository_service::Error::Precondition(_) => ErrorCode::PreconditionFailed,
                    alt_repository_service::Error::Forbidden(_) => ErrorCode::Forbidden,
                    alt_repository_service::Error::Limit(_) => ErrorCode::QuotaExceeded,
                    _ => ErrorCode::IntegrityError,
                };
                reject(&mut tx, code, error.to_string())?;
                tx.commit().map_err(|_| Error::OutcomeUnknown)?;
                return Err(error.into());
            }
        };
        check_graph_pins(&mut tx, work, graph)?;
        crate::change_repair::claim_published_tasks(
            &mut tx,
            &self.spec,
            work,
            tasks,
            &current,
            &auth,
            &row.get::<_, String>("digest"),
        )?;
        let repository = &work.key.scope.repository_instance_id;
        refs::replace(&mut tx, repository, &validated.updates)?;
        let unreachable = work
            .publication
            .refs
            .iter()
            .any(|update| update.force || update.new.is_none());
        let sequence=unsigned(tx.query_one("UPDATE alt_repositories SET transaction_sequence=transaction_sequence+1,pack_unreachable=pack_unreachable OR $2 WHERE id=$1 RETURNING transaction_sequence",&[repository,&unreachable])?.get(0))?;
        crate::tasks::retain(&mut tx, work, tasks, sequence, &validated.digest)?;
        let body = ReceiptBody {
            publication: work.publication.clone(),
            request_digest: validated.digest,
            transaction_sequence: sequence,
            policy_version: validated.policy_version,
            committed_at_unix_ms: now(&mut tx)?,
        };
        let signature = self.signing_key.sign(&body.signing_bytes(self.limits)?);
        let receipt = Receipt {
            body,
            signing_key_id: alt_hash::blake3::hash(self.signing_key.verifying_key().as_bytes())
                .to_hex()
                .to_string(),
            signature: signature
                .to_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        };
        let result = RequestState::Committed {
            receipt: Box::new(receipt.clone()),
        };
        let event = serde_json::json!({"kind":"publication_committed","receipt":receipt});
        tx.execute(
            "INSERT INTO alt_audit(repository,sequence,actor_id,event) VALUES($1,$2,$3,$4)",
            &[repository, &integer(sequence)?, &auth.actor_id, &event],
        )?;
        tx.execute(
            "INSERT INTO alt_outbox(repository,sequence,event) VALUES($1,$2,$3)",
            &[repository, &integer(sequence)?, &event],
        )?;
        finish(
            &mut tx,
            &work.key,
            "committed",
            &result,
            unsigned(row.get("reserved_bytes"))?,
        )?;
        // With the upload pins gone, what this request uploaded and nothing retains
        // leaves the content store: its Git objects, which the segments hold now, and
        // the chunks its content was assembled from. The new segment is counted instead.
        let uploaded: BTreeSet<&str> = work
            .manifest
            .entries
            .iter()
            .flat_map(|entry| {
                std::iter::once(entry.blake3.as_str())
                    .chain(entry.chunks.iter().map(|chunk| chunk.blake3.as_str()))
            })
            .collect();
        crate::pack_native::release_content(
            &mut tx,
            repository,
            &uploaded.into_iter().collect::<Vec<_>>(),
        )?;
        if let Some(segment) = materialized {
            crate::pack_native::account_segment(&mut tx, repository, segment.bytes)?;
            crate::packs::insert_segment(
                &mut tx,
                repository,
                sequence,
                &crate::NewSegment {
                    id: &segment.id,
                    objects: segment.objects,
                    bytes: segment.bytes,
                    flags: segment.flags,
                    inline: segment.inline.as_deref(),
                },
            )?;
        }
        // SQL work and lock waits can outlive a lease checked on entry.
        crate::store::credential_row(&mut tx, &self.spec, credential)?;
        crate::tasks::recheck_current_authority(&mut tx, &self.spec, work, tasks, &current)?;
        crate::cloud_quota::check_publication(&mut tx, &self.spec)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(receipt)
    }
}
