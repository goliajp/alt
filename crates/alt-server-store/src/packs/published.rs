//! The publication of a push: its refs and the segment of its objects.

use super::*;

impl Store {
    /// The live segments of `repository` as they are now, with their ids: what a
    /// push is verified behind anew whose publication found them moved.
    pub fn segments_now(&mut self, repository: &str) -> Result<(Vec<String>, SegmentSet)> {
        let (ids, segments) = self.live_segments(repository)?;
        Ok((ids.into_iter().map(|(id, _)| id).collect(), segments))
    }

    /// Publishes ref updates, and the segment that carries their new objects, in one
    /// transaction. The segment file must already be durable under its final name.
    ///
    /// The segment was verified behind the live segments `behind`: the depth of
    /// its deltas and whether it brings an object again are told of those. With
    /// the row of the repository locked the live segments are held against them,
    /// and where a push or a merge was published meanwhile nothing is published
    /// ([`crate::SEGMENTS_MOVED`]): the caller verifies the pack behind the
    /// segments as they are now. The file of the segment is looked for under
    /// that lock too, which a sweep holds while it removes files no row knows.
    pub fn pack_publish(
        &mut self,
        credential: &Credential,
        publication: &Publication,
        graph: &impl Graph,
        segment: Option<NewSegment<'_>>,
        behind: &[String],
    ) -> Result<Receipt> {
        let repository = &publication.key.scope.repository_instance_id;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let live = Self::live_segment_ids(&mut tx, repository)?;
        if !live.iter().map(|(id, _)| id).eq(behind) {
            return Err(Error::Precondition(crate::SEGMENTS_MOVED));
        }
        let held = (tx, &current, self.bytes.root());
        published(
            held,
            &self.spec,
            &self.signing_key,
            self.limits,
            credential,
            publication,
            graph,
            segment,
        )
    }

    /// [`Self::pack_publish`] behind whatever segments are live once the row of
    /// the repository is locked: `made` is given them with their ids, and gives
    /// what is published behind them, the segments with the new one among them
    /// and that one. No push and no merge is published while it does: what it
    /// does is to be short. This is how a push is published that found the
    /// segments moved time after time.
    pub fn pack_publish_held(
        &mut self,
        credential: &Credential,
        repository: &str,
        made: &mut dyn FnMut(&[String], &SegmentSet) -> Result<HeldPublication>,
    ) -> Result<Receipt> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let live = Self::live_segment_ids(&mut tx, repository)?;
        let root = self.bytes.root();
        let behind = opened(root, repository, &live, &mut |id| {
            Ok(tx.query_one(INLINE, &[&repository, &id])?.get(0))
        })?;
        let ids: Vec<String> = live.into_iter().map(|(id, _)| id).collect();
        let (publication, set, segment) = made(&ids, &behind)?;
        if publication.key.scope.repository_instance_id != repository {
            return Err(Error::Unauthorized);
        }
        let graph = SegmentGraph {
            set: &set,
            limits: self.limits.into(),
        };
        let segment = segment.as_ref().map(|segment| NewSegment {
            id: &segment.id,
            objects: segment.objects,
            bytes: segment.bytes,
            flags: segment.flags,
            inline: segment.inline.as_deref(),
        });
        let held = (tx, &current, root);
        published(
            held,
            &self.spec,
            &self.signing_key,
            self.limits,
            credential,
            &publication,
            &graph,
            segment,
        )
    }
}

/// What [`Store::pack_publish_held`] is given to publish: the publication, the
/// segments with the new one among them, and that one.
pub type HeldPublication = (Publication, SegmentSet, Option<HeldSegment>);

/// A segment as [`Store::pack_publish_held`] is given it.
pub struct HeldSegment {
    pub id: String,
    pub objects: u64,
    pub bytes: u64,
    pub flags: u32,
    pub inline: Option<Vec<u8>>,
}

/// The rest of the publication of a push, in the transaction that holds the row
/// of its repository, which is `current`.
#[expect(
    clippy::too_many_arguments,
    reason = "the parts of a store, apart for the transaction that borrows one of them"
)]
fn published(
    (mut tx, current, root): (
        crate::database::Transaction<'_>,
        &alt_repository_service::RepositorySnapshot,
        &std::path::Path,
    ),
    spec: &crate::InstanceSpec,
    signing_key: &alt_crypto::ed25519::SigningKey,
    limits: alt_remote_protocol::Limits,
    credential: &Credential,
    publication: &Publication,
    graph: &impl Graph,
    segment: Option<NewSegment<'_>>,
) -> Result<Receipt> {
    let repository = &publication.key.scope.repository_instance_id;
    let auth = authorize(&mut tx, spec, credential, current)?;
    let time = now(&mut tx)?;
    let validated = validate_publication(publication, current, &auth, graph, limits, time)?;
    crate::publication::replace_refs(&mut tx, repository, &validated.updates)?;
    let unreachable = publication
        .refs
        .iter()
        .any(|update| update.force || update.new.is_none());
    let sequence = unsigned(
            tx.query_one(
                "UPDATE alt_repositories SET transaction_sequence=transaction_sequence+1,pack_unreachable=pack_unreachable OR $2 WHERE id=$1 RETURNING transaction_sequence",
                &[repository, &unreachable],
            )?
            .get(0),
        )?;
    if let Some(segment) = &segment {
        if !segment_file(root, repository, segment.id)?.is_file() {
            return Err(Error::Integrity("pack segment file is gone"));
        }
        if tx.execute(
                "UPDATE alt_repositories SET stored_bytes=stored_bytes+$2 WHERE id=$1 AND $2<=quota_bytes-stored_bytes-reserved_bytes-link_bytes",
                &[repository, &integer(segment.bytes)?],
            )? != 1
            {
                return Err(Error::Limit("repository quota"));
            }
        insert_segment(&mut tx, repository, sequence, segment)?;
    }
    let body = ReceiptBody {
        publication: publication.clone(),
        request_digest: validated.digest.clone(),
        transaction_sequence: sequence,
        policy_version: validated.policy_version,
        committed_at_unix_ms: now(&mut tx)?,
    };
    let signature = signing_key.sign(&body.signing_bytes_of_validated()?);
    let receipt = Receipt {
        body,
        signing_key_id: alt_hash::blake3::hash(signing_key.verifying_key().as_bytes())
            .to_hex()
            .to_string(),
        signature: signature
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    };
    let documents = crate::committed::committed_documents(&receipt)?;
    crate::committed::record_committed(&mut tx, repository, sequence, &auth.actor_id, &documents)?;
    tx.execute(
            "INSERT INTO alt_requests(repository,epoch,actor_id,request_id,digest,publication,manifest,state,result,expires_at,reserved_bytes,settled_at) VALUES($1,$2,$3,$4,$5,$6,$7,'committed',$8,0,0,floor(extract(epoch FROM clock_timestamp())*1000)::bigint)",
            &[
                repository,
                &integer(publication.key.scope.recovery_epoch)?,
                &publication.key.actor_id,
                &publication.key.request_id,
                &validated.digest,
                &JsonText(&alt_json::to_vec(publication)?),
                &JsonText(&alt_json::to_vec(&alt_json::json!({"pack_segment": segment.as_ref().map(|segment| segment.id)}))?),
                &JsonText(&documents.result),
            ],
        )?;
    // lock waits can outlive a lease checked on entry
    credential_row(&mut tx, spec, credential)?;
    crate::cloud_quota::check_publication(&mut tx, spec)?;
    tx.commit().map_err(|_| Error::OutcomeUnknown)?;
    Ok(receipt)
}
