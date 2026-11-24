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
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let time = now(&mut tx)?;
        let validated =
            validate_publication(publication, &current, &auth, graph, self.limits, time)?;
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
        let live = Self::live_segment_ids(&mut tx, repository)?;
        if !live.iter().map(|(id, _)| id).eq(behind) {
            return Err(Error::Precondition(crate::SEGMENTS_MOVED));
        }
        if let Some(segment) = &segment {
            if !segment_file(self.bytes.root(), repository, segment.id)?.is_file() {
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
        let signature = self.signing_key.sign(&body.signing_bytes_of_validated()?);
        let receipt = Receipt {
            body,
            signing_key_id: alt_hash::blake3::hash(self.signing_key.verifying_key().as_bytes())
                .to_hex()
                .to_string(),
            signature: signature
                .to_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        };
        let documents = crate::committed::committed_documents(&receipt)?;
        crate::committed::record_committed(
            &mut tx,
            repository,
            sequence,
            &auth.actor_id,
            &documents,
        )?;
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
        credential_row(&mut tx, &self.spec, credential)?;
        crate::cloud_quota::check_publication(&mut tx, &self.spec)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(receipt)
    }
}
