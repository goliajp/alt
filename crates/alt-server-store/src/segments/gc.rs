use super::*;
use crate::store::require_admin;

impl Store {
    pub(crate) fn collect_segments(
        &mut self,
        admin: &Credential,
        repository: &str,
        limit: u32,
    ) -> Result<usize> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        require_admin(&mut tx, &self.spec, admin)?;
        let time = integer(now(&mut tx)?)?;
        let rows = tx.query("SELECT s.segment_id FROM alt_segments s WHERE s.repository=$1 AND NOT EXISTS(SELECT 1 FROM alt_blob_locations l WHERE l.repository=s.repository AND l.segment_id=s.segment_id) AND (s.state IN ('stored','deleting') OR (s.expires_at<=$2 AND (s.owner_kind='compact' OR NOT EXISTS(SELECT 1 FROM alt_requests q JOIN alt_repositories r ON r.id=q.repository WHERE q.repository=s.repository AND q.epoch=s.epoch AND q.actor_id=s.actor_id AND q.request_id=s.request_id AND q.epoch=r.recovery_epoch AND q.state IN ('receiving','ready') AND q.expires_at>$2)))) ORDER BY s.segment_id LIMIT $3", &[&repository,&time,&i64::from(limit)])?;
        let ids: Vec<String> = rows.into_iter().map(|row| row.get(0)).collect();
        tx.execute(
            "UPDATE alt_segments SET state='deleting' WHERE repository=$1 AND segment_id=ANY($2)",
            &[&repository, &ids],
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        let mut removed = 0;
        for id in ids {
            let mut tx = self.client.transaction()?;
            instance(&mut tx, &self.spec, &self.signing_key)?;
            snapshot(&mut tx, &self.spec, repository)?;
            require_admin(&mut tx, &self.spec, admin)?;
            let Some(row) = tx.query_opt("SELECT state,EXISTS(SELECT 1 FROM alt_blob_locations l WHERE l.repository=s.repository AND l.segment_id=s.segment_id) FROM alt_segments s WHERE repository=$1 AND segment_id=$2", &[&repository,&id])? else { continue; };
            if row.get::<_, String>(0) != "deleting" || row.get::<_, bool>(1) {
                return Err(Error::Precondition("segment GC marker changed"));
            }
            self.bytes.remove_segment(&current.scope, &id)?;
            tx.execute(
                "DELETE FROM alt_segments WHERE repository=$1 AND segment_id=$2",
                &[&repository, &id],
            )?;
            tx.commit().map_err(|_| Error::OutcomeUnknown)?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Compact sparse immutable segments without moving any currently pinned
    /// lazy reader. Both replacement creation and location switch are bounded
    /// by the existing repository/instance barriers; old markers survive errors.
    pub fn compact_segments(
        &mut self,
        admin: &Credential,
        repository: &str,
        limit: u32,
    ) -> Result<usize> {
        if limit == 0 || limit > 8 {
            return Err(Error::Limit("segment compaction batch"));
        }
        let mut completed = 0;
        for _ in 0..limit {
            let mut tx = self.client.transaction()?;
            instance(&mut tx, &self.spec, &self.signing_key)?;
            snapshot(&mut tx, &self.spec, repository)?;
            require_admin(&mut tx, &self.spec, admin)?;
            let time = now(&mut tx)?;
            let attempts: i64 = tx.query_one("SELECT count(*) FROM alt_segments WHERE repository=$1 AND owner_kind='compact' AND state='uploading'", &[&repository])?.get(0);
            if attempts >= 8 {
                break;
            }
            // Aggregate once, then consider at most one bounded source segment.
            // Byte sparsity handles large dead members; count sparsity also
            // reclaims empty objects and their index/storage overhead.
            let candidate = tx.query_opt("SELECT s.segment_id,count(l.blake3),COALESCE(sum(b.bytes),0)::bigint FROM alt_segments s JOIN alt_blob_locations l USING(repository,segment_id) JOIN alt_blobs b ON b.repository=l.repository AND b.blake3=l.blake3 WHERE s.repository=$1 AND s.state='stored' AND NOT EXISTS(SELECT 1 FROM alt_blob_locations x JOIN alt_pins p ON p.repository=x.repository AND p.blake3=x.blake3 WHERE x.repository=s.repository AND x.segment_id=s.segment_id AND ((p.expires_at IS NOT NULL AND p.expires_at>$2) OR p.pin_id LIKE 'portable-export:%')) GROUP BY s.segment_id,s.member_count,s.payload_bytes HAVING count(l.blake3)*2<s.member_count OR COALESCE(sum(b.bytes),0)*2<s.payload_bytes ORDER BY s.segment_id LIMIT 1", &[&repository,&integer(time)?])?;
            let Some(candidate) = candidate else {
                break;
            };
            let old: String = candidate.get(0);
            let id = new_id();
            let expires = time
                .checked_add(300_000)
                .ok_or(Error::Limit("compaction expiry"))?;
            tx.execute("INSERT INTO alt_segments(repository,segment_id,owner_kind,state,expires_at,member_count,payload_bytes,selection) VALUES($1,$2,'compact','uploading',$3,$4,$5,'{}')", &[&repository,&id,&integer(expires)?,&candidate.get::<_, i64>(1),&candidate.get::<_, i64>(2)])?;
            tx.commit().map_err(|_| Error::OutcomeUnknown)?;

            let mut tx = self.client.transaction()?;
            instance(&mut tx, &self.spec, &self.signing_key)?;
            let current = snapshot(&mut tx, &self.spec, repository)?;
            require_admin(&mut tx, &self.spec, admin)?;
            let time = now(&mut tx)?;
            let allowed: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM alt_segments WHERE repository=$1 AND segment_id=$2 AND owner_kind='compact' AND state='uploading' AND expires_at>$4) AND EXISTS(SELECT 1 FROM alt_segments WHERE repository=$1 AND segment_id=$3 AND state='stored') AND NOT EXISTS(SELECT 1 FROM alt_blob_locations l JOIN alt_pins p USING(repository,blake3) WHERE l.repository=$1 AND l.segment_id=$3 AND ((p.expires_at IS NOT NULL AND p.expires_at>$4) OR p.pin_id LIKE 'portable-export:%'))", &[&repository,&id,&old,&integer(time)?])?.get(0);
            if !allowed {
                tx.execute("UPDATE alt_segments SET state='deleting' WHERE repository=$1 AND segment_id=$2 AND owner_kind='compact' AND state='uploading'", &[&repository,&id])?;
                tx.commit().map_err(|_| Error::OutcomeUnknown)?;
                continue;
            }
            let members: BTreeMap<String,u64> = tx.query("SELECT l.blake3,b.bytes FROM alt_blob_locations l JOIN alt_blobs b USING(repository,blake3) WHERE l.repository=$1 AND l.segment_id=$2 AND b.state='stored' ORDER BY l.blake3", &[&repository,&old])?.into_iter().map(|row| Ok((row.get(0),unsigned(row.get(1))?))).collect::<Result<_>>()?;
            if members.is_empty() {
                tx.execute("UPDATE alt_segments SET state='deleting' WHERE repository=$1 AND segment_id=$2", &[&repository,&id])?;
                tx.commit().map_err(|_| Error::OutcomeUnknown)?;
                continue;
            }
            let digests: Vec<_> = members.keys().map(String::as_str).collect();
            let selected = locations(&mut tx, repository, &digests)?;
            let mut readers = Vec::with_capacity(members.len());
            for (digest, size) in &members {
                let mut reader = self.bytes.verify_payload(
                    &current.scope,
                    digest,
                    *size,
                    selected.get(digest),
                )?;
                if let Some(first) = readers.first() {
                    reader.share_backing(first)?;
                }
                readers.push(reader);
            }
            let mut inputs: Vec<_> = members
                .iter()
                .zip(readers.iter_mut())
                .map(|((digest, size), source)| {
                    Ok(alt_byte_segment::Input {
                        digest: *blake3::Hash::from_hex(digest)
                            .map_err(|_| Error::Integrity("compaction digest"))?
                            .as_bytes(),
                        bytes: *size,
                        source: source as &mut dyn std::io::Read,
                    })
                })
                .collect::<Result<_>>()?;
            let proof = self.bytes.put_segment(&current.scope, &id, &mut inputs)?;
            // Compaction has no upload-registration step. Verify the installed
            // container against its encoder proof before changing any location.
            self.bytes.check_segment_proof(&proof)?;
            let index = blake3::Hash::from(*proof.index().digest())
                .to_hex()
                .to_string();
            tx.execute("UPDATE alt_segments SET state='stored',index_digest=$3,selection=$4,member_count=$5,payload_bytes=$6 WHERE repository=$1 AND segment_id=$2", &[&repository,&id,&index,&serde_json::to_value(&members)?,&integer(members.len() as u64)?,&integer(proof.index().bytes())?])?;
            let offsets: Vec<_> = members
                .keys()
                .map(|digest| {
                    let hash = blake3::Hash::from_hex(digest)
                        .map_err(|_| Error::Integrity("compaction digest"))?;
                    integer(
                        proof
                            .location(hash.as_bytes())
                            .ok_or(Error::Integrity("compaction member"))?
                            .offset,
                    )
                })
                .collect::<Result<_>>()?;
            if tx.execute("UPDATE alt_blob_locations l SET segment_id=$2,index_digest=$3,byte_offset=v.byte_offset FROM unnest($4::text[],$5::bigint[]) v(blake3,byte_offset) WHERE l.repository=$1 AND l.blake3=v.blake3 AND l.segment_id=$6", &[&repository,&id,&index,&digests,&offsets,&old])? != members.len() as u64 {
                return Err(Error::Precondition("compaction locations changed"));
            }
            // Deleting logical members may still reference the old file. GC
            // releases those locations before reclaiming its physical marker.
            let remaining: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM alt_blob_locations WHERE repository=$1 AND segment_id=$2)", &[&repository,&old])?.get(0);
            if !remaining {
                tx.execute("UPDATE alt_segments SET state='deleting' WHERE repository=$1 AND segment_id=$2", &[&repository,&old])?;
            }
            require_admin(&mut tx, &self.spec, admin)?;
            if now(&mut tx)? >= expires {
                return Err(Error::Precondition("compaction intent expired"));
            }
            tx.commit().map_err(|_| Error::OutcomeUnknown)?;
            completed += 1;
        }
        Ok(completed)
    }
}
