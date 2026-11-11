//! Physical segment ownership and locations. All mutations share the repository
//! barrier with logical publication and garbage collection.
use crate::Jsonb;
use crate::database::Transaction;
use crate::requests::{active, check_key, upload_request_row};
use crate::store::{authorize, instance, integer, now, snapshot, unsigned};
use crate::{
    ByteInput, ByteStore, Credential, DurableBytes, Error, InstanceSpec, Result, SegmentLocation,
    Store,
};
use alt_crypto::ed25519::SigningKey;
use alt_remote_protocol::{ContentEntry, Limits, Publication, RequestKey};
use std::collections::BTreeMap;

pub(crate) type Locations = BTreeMap<String, SegmentLocation>;
pub(crate) fn locations(
    tx: &mut Transaction<'_>,
    repository: &str,
    digests: &[&str],
) -> Result<Locations> {
    let mut result = BTreeMap::new();
    for batch in digests.chunks(crate::database::BATCH_ROWS) {
        // Select exact requested keys before looking up their containers. With
        // fresh statistics, joining containers first can scan every member of
        // a segment and compare each against the complete generic array.
        // The caller's repository barrier covers both bounded queries.
        let rows = tx.query(
            "SELECT l.blake3,l.segment_id,l.index_digest,l.byte_offset FROM (SELECT DISTINCT unnest($2::text[]) AS blake3) AS wanted JOIN alt_blob_locations l ON l.repository=$1 AND l.blake3=wanted.blake3",
            &[&repository, &batch],
        )?;
        let ids: std::collections::BTreeSet<String> = rows.iter().map(|row| row.get(1)).collect();
        let ids: Vec<_> = ids.iter().map(String::as_str).collect();
        let segments: BTreeMap<String, (String, Option<String>)> = if ids.is_empty() {
            BTreeMap::new()
        } else {
            tx.query(
                "SELECT segment_id,state,index_digest FROM alt_segments WHERE repository=$1 AND segment_id=ANY($2)",
                &[&repository, &ids],
            )?
            .into_iter()
            .map(|row| (row.get(0), (row.get(1), row.get(2))))
            .collect()
        };
        for row in rows {
            let segment_id: String = row.get(1);
            let index: String = row.get(2);
            let (state, expected) = segments
                .get(&segment_id)
                .ok_or(Error::Integrity("segment location is not stored"))?;
            if state != "stored" || expected.as_ref() != Some(&index) {
                return Err(Error::Integrity("segment location is not stored"));
            }
            let hash = alt_hash::blake3::Hash::from_hex(&index)
                .map_err(|_| Error::Integrity("segment index digest"))?;
            result.insert(
                row.get(0),
                SegmentLocation {
                    segment_id,
                    index_digest: *hash.as_bytes(),
                    offset: unsigned(row.get(3))?,
                },
            );
        }
    }
    Ok(result)
}
pub(crate) fn location(
    tx: &mut Transaction<'_>,
    repository: &str,
    digest: &str,
) -> Result<Option<SegmentLocation>> {
    Ok(locations(tx, repository, &[digest])?.remove(digest))
}
fn upload_authority(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    signing_key: &SigningKey,
    credential: &Credential,
    key: &RequestKey,
    limits: Limits,
) -> Result<postgres::Row> {
    instance(tx, spec, signing_key)?;
    let current = snapshot(tx, spec, &key.scope.repository_instance_id)?;
    let auth = authorize(tx, spec, credential, &current)?;
    check_key(key, &current.scope, &auth.actor_id)?;
    let request =
        upload_request_row(tx, key)?.ok_or(Error::Precondition("segment request missing"))?;
    let time = now(tx)?;
    active(&request, time)?;
    let publication: Publication = request.get::<_, Jsonb>("publication").read()?;
    alt_repository_service::authorize_intent(&publication, &current, &auth, limits, time)?;
    Ok(request)
}
pub(crate) fn new_id() -> String {
    let mut bytes = [0; 16];
    alt_os::random::fill(&mut bytes);
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
impl Store {
    pub(crate) fn selected_locations(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digests: &[&str],
    ) -> Result<Locations> {
        let mut tx = self.client.transaction()?;
        upload_authority(
            &mut tx,
            &self.spec,
            &self.signing_key,
            credential,
            key,
            self.limits,
        )?;
        let result = locations(&mut tx, &key.scope.repository_instance_id, digests)?;
        tx.commit()?;
        Ok(result)
    }
    /// Persist selected distinct contents under a precommitted bounded intent.
    /// The second transaction fences GC through file creation/installation, and
    /// rechecks the same authority before releasing the physical proof.
    pub fn persist_contents(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        inputs: &mut [ByteInput<'_>],
    ) -> Result<Vec<DurableBytes>> {
        if inputs.is_empty() || inputs.len() > alt_byte_segment::MAX_MEMBERS {
            return Err(Error::Limit("segment members"));
        }
        let mut selection = BTreeMap::new();
        let mut total = 0u64;
        for input in inputs.iter() {
            crate::bytes::check_digest(input.digest)?;
            total = total
                .checked_add(input.bytes)
                .ok_or(Error::Limit("segment bytes"))?;
            if input.bytes > self.limits.object_bytes
                || total > alt_byte_segment::MAX_BYTES
                || selection
                    .insert(input.digest.to_owned(), input.bytes)
                    .is_some()
            {
                return Err(Error::Limit("distinct bounded segment inputs"));
            }
        }
        let repository = &key.scope.repository_instance_id;
        let id = new_id();
        let mut tx = self.client.transaction()?;
        let request = upload_authority(
            &mut tx,
            &self.spec,
            &self.signing_key,
            credential,
            key,
            self.limits,
        )?;
        let digests: Vec<_> = selection.keys().map(String::as_str).collect();
        let rows = tx.query("SELECT c.blake3,c.selected_entries,b.bytes,b.state FROM alt_request_content c JOIN alt_blobs b USING(repository,blake3) WHERE c.repository=$1 AND c.epoch=$2 AND c.actor_id=$3 AND c.request_id=$4 AND c.blake3=ANY($5)", &[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&digests])?;
        if rows.len() != selection.len() {
            return Err(Error::Unauthorized);
        }
        for row in rows {
            let digest: String = row.get(0);
            let entries: Vec<ContentEntry> = row.get::<_, Jsonb>(1).read()?;
            if row.get::<_, String>(3) == "deleting"
                || unsigned(row.get(2))? != selection[&digest]
                || !entries
                    .iter()
                    .any(|entry| entry.blake3 == digest && entry.bytes == selection[&digest])
            {
                return Err(Error::Precondition("segment selection or GC fence"));
            }
        }
        let pending: i64 = tx.query_one("SELECT count(*) FROM alt_segments WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 AND state='uploading'", &[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?.get(0);
        if pending >= 8 {
            return Err(Error::Limit("pending segment attempts"));
        }
        tx.execute("INSERT INTO alt_segments(repository,segment_id,owner_kind,epoch,actor_id,request_id,state,expires_at,member_count,payload_bytes,selection) VALUES($1,$2,'upload',$3,$4,$5,'uploading',$6,$7,$8,$9)", &[repository,&id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&request.get::<_, i64>("expires_at"),&integer(selection.len() as u64)?,&integer(total)?,&Jsonb::from(&selection)?])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;

        let mut tx = self.client.transaction()?;
        upload_authority(
            &mut tx,
            &self.spec,
            &self.signing_key,
            credential,
            key,
            self.limits,
        )?;
        let intent = tx
            .query_opt(
                "SELECT state,expires_at FROM alt_segments WHERE repository=$1 AND segment_id=$2",
                &[repository, &id],
            )?
            .ok_or(Error::Precondition("segment intent was collected"))?;
        if intent.get::<_, String>(0) != "uploading" || unsigned(intent.get(1))? <= now(&mut tx)? {
            return Err(Error::Precondition("segment intent expired"));
        }
        // Re-read after the precommitted intent: another request may have
        // installed the same canonical member in the intervening transaction.
        let states: BTreeMap<String, (String, u64)> = tx
            .query(
                "SELECT blake3,state,bytes FROM alt_blobs WHERE repository=$1 AND blake3=ANY($2)",
                &[repository, &digests],
            )?
            .into_iter()
            .map(|row| Ok((row.get(0), (row.get(1), unsigned(row.get(2))?))))
            .collect::<Result<_>>()?;
        let current_locations = locations(&mut tx, repository, &digests)?;
        let mut proofs = Vec::new();
        let mut new_selection = BTreeMap::new();
        for input in inputs.iter_mut() {
            let (state, bytes) = states
                .get(input.digest)
                .ok_or(Error::Precondition("selected content disappeared"))?;
            if state == "deleting" || *bytes != input.bytes {
                return Err(Error::Precondition("selected content changed"));
            }
            if state == "stored" {
                let location = current_locations.get(input.digest);
                let proof = self.bytes.existing_proof(&key.scope, input, location)?;
                proofs.push(proof);
            } else {
                new_selection.insert(input.digest.to_owned(), input.bytes);
            }
        }
        if new_selection.is_empty() {
            tx.execute(
                "UPDATE alt_segments SET state='deleting' WHERE repository=$1 AND segment_id=$2",
                &[repository, &id],
            )?;
        } else {
            let total: u64 = new_selection.values().sum();
            tx.execute("UPDATE alt_segments SET selection=$3,member_count=$4,payload_bytes=$5 WHERE repository=$1 AND segment_id=$2", &[repository,&id,&Jsonb::from(&new_selection)?,&integer(new_selection.len() as u64)?,&integer(total)?])?;
            let mut streams: Vec<_> = inputs
                .iter_mut()
                .filter(|input| new_selection.contains_key(input.digest))
                .map(|input| {
                    Ok(alt_byte_segment::Input {
                        digest: *alt_hash::blake3::Hash::from_hex(input.digest)
                            .map_err(|_| Error::Integrity("segment digest"))?
                            .as_bytes(),
                        bytes: input.bytes,
                        source: &mut *input.source,
                    })
                })
                .collect::<Result<_>>()?;
            let segment = self.bytes.put_segment(&key.scope, &id, &mut streams)?;
            proofs.extend(segment.proofs());
        }
        upload_authority(
            &mut tx,
            &self.spec,
            &self.signing_key,
            credential,
            key,
            self.limits,
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(proofs)
    }
}

/// Called inside the same transaction as logical object mapping and accounting.
/// Existing stored blobs retain their original location, including loose data.
pub(crate) fn register_locations(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
    entries: &[(&ContentEntry, &DurableBytes)],
    bytes: &ByteStore,
) -> Result<()> {
    let mut groups: BTreeMap<&str, Vec<(&ContentEntry, &DurableBytes)>> = BTreeMap::new();
    let digests: Vec<_> = entries
        .iter()
        .filter(|(_, proof)| proof.existing())
        .map(|(entry, _)| entry.blake3.as_str())
        .collect();
    let current = locations(tx, &key.scope.repository_instance_id, &digests)?;
    let stored: std::collections::BTreeSet<String> = tx.query(
        "SELECT blake3 FROM alt_blobs WHERE repository=$1 AND blake3=ANY($2) AND state='stored'",
        &[&key.scope.repository_instance_id, &digests],
    )?.into_iter().map(|row| row.get(0)).collect();
    for &(entry, proof) in entries {
        if proof.existing() {
            if !stored.contains(&entry.blake3) || current.get(&entry.blake3) != proof.location() {
                return Err(Error::Precondition("existing content location changed"));
            }
            continue;
        }
        if let Some(location) = proof.location() {
            groups
                .entry(&location.segment_id)
                .or_default()
                .push((entry, proof));
        }
    }
    for (id, members) in groups {
        let row = tx.query_opt("SELECT owner_kind,epoch,actor_id,request_id,state,expires_at,selection,index_digest FROM alt_segments WHERE repository=$1 AND segment_id=$2", &[&key.scope.repository_instance_id,&id])?.ok_or(Error::Precondition("segment intent missing"))?;
        if row.get::<_, String>(0) != "upload"
            || row.get::<_, Option<i64>>(1) != Some(integer(key.scope.recovery_epoch)?)
            || row.get::<_, Option<String>>(2).as_ref() != Some(&key.actor_id)
            || row.get::<_, Option<String>>(3).as_ref() != Some(&key.request_id)
            || row.get::<_, String>(4) == "deleting"
            || unsigned(row.get(5))? <= now(tx)?
        {
            return Err(Error::Precondition("segment owner or lease changed"));
        }
        let expected: BTreeMap<String, u64> = row.get::<_, Jsonb>(6).read()?;
        let actual: BTreeMap<String, u64> = members
            .iter()
            .map(|(entry, _)| (entry.blake3.clone(), entry.bytes))
            .collect();
        if actual != expected {
            return Err(Error::Integrity("segment registration selection"));
        }
        let index = members[0].1.location().unwrap().index_digest;
        let checksum = members[0]
            .1
            .segment_checksum()
            .ok_or(Error::Integrity("missing segment encoding proof"))?;
        let encoded = alt_hash::blake3::Hash::from(index).to_hex().to_string();
        if members.iter().any(|(_, proof)| {
            proof.location().unwrap().index_digest != index
                || proof.segment_checksum() != Some(checksum)
        }) || row
            .get::<_, Option<String>>(7)
            .as_ref()
            .is_some_and(|old| old != &encoded)
        {
            return Err(Error::Integrity("segment registration index"));
        }
        bytes.verify_segment_layout(&key.scope, id, &index, &expected, checksum)?;
        tx.execute("UPDATE alt_segments SET state='stored',index_digest=$3 WHERE repository=$1 AND segment_id=$2", &[&key.scope.repository_instance_id,&id,&encoded])?;
        let mut unique = BTreeMap::new();
        for (entry, proof) in members {
            unique.insert(entry.blake3.as_str(), proof.location().unwrap().offset);
        }
        let digests: Vec<_> = unique.keys().copied().collect();
        let offsets: Vec<_> = unique
            .values()
            .map(|v| integer(*v))
            .collect::<Result<_>>()?;
        tx.execute("INSERT INTO alt_blob_locations(repository,blake3,segment_id,index_digest,byte_offset) SELECT $1,v.blake3,$2,$3,v.byte_offset FROM unnest($4::text[],$5::bigint[]) v(blake3,byte_offset) JOIN alt_blobs b ON b.repository=$1 AND b.blake3=v.blake3 WHERE b.state='staging' ON CONFLICT DO NOTHING", &[&key.scope.repository_instance_id,&id,&encoded,&digests,&offsets])?;
    }
    Ok(())
}

mod gc;
