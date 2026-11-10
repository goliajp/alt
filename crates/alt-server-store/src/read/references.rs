//! Bounded reference/tag projection. Ordinary fetch/export snapshots remain full.
use super::*;
use alt_repository_service::{Node, verify_object};

impl Store {
    /// Pin only matching ref tips and their annotated-tag chains. Commit/tree
    /// descendants are not selected: this snapshot is for listing refs, not
    /// fetch or export. Tag originals are verified outside database locks;
    /// each expansion pins current published targets under the repository
    /// barrier. Callers must still buffer output with `with_snapshot_objects`.
    pub fn read_ref_snapshot(
        &mut self,
        credential: &Credential,
        repository: &str,
        ttl_ms: u64,
        prefixes: &[String],
    ) -> Result<ReadSnapshot> {
        let mut read =
            self.read_snapshot_inner(credential, repository, ttl_ms, Selection::RefTips(prefixes))?;
        let result = self.select_ref_targets(credential, &mut read);
        if let Err(error) = result {
            // As for normal reads, failed cleanup leaves only expiring pins.
            let _ = self.release_snapshot(credential, read);
            return Err(error);
        }
        Ok(read)
    }

    fn select_ref_targets(
        &mut self,
        credential: &Credential,
        read: &mut ReadSnapshot,
    ) -> Result<()> {
        let mut pending: BTreeSet<_> = read.objects.keys().copied().collect();
        let mut visited = BTreeSet::new();
        let mut remaining = self.limits.walk_steps;
        let mut bytes_left = self.limits.total_bytes;
        let limits = self.limits;
        while !pending.is_empty() {
            remaining = remaining
                .checked_sub(pending.len())
                .ok_or(Error::Limit("reference tag walk"))?;
            let tags: Vec<_> = pending
                .iter()
                .copied()
                .filter(|id| read.objects[id].0 == ObjectKind::Tag)
                .collect();
            if tags.is_empty() {
                break;
            }
            for id in &tags {
                bytes_left = bytes_left
                    .checked_sub(read.objects[id].2)
                    .ok_or(Error::Limit("reference tag bytes"))?;
            }
            let targets = self.with_snapshot_objects(
                credential,
                read,
                |objects| {
                    let mut targets = Vec::new();
                    for id in &tags {
                        let object = objects
                            .open(*id)?
                            .ok_or(Error::Integrity("reference tag missing"))?;
                        let Node::Tag { target, kind } = verify_object(*id, object, limits.into())?
                        else {
                            return Err(Error::Integrity("reference tag kind"));
                        };
                        targets.push((target, kind));
                    }
                    Ok(targets)
                },
                |_| Ok(credential.clone()),
            )?;
            visited.extend(pending);
            let wanted = targets.iter().map(|(id, _)| *id).collect();
            self.extend_ref_snapshot(credential, read, &wanted)?;
            for (id, kind) in &targets {
                if read.objects[id].0 != *kind {
                    return Err(Error::Integrity("reference tag target kind"));
                }
            }
            pending = targets
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| !visited.contains(id))
                .collect();
        }
        // Each selection transaction rechecks authority and expiry before
        // committing pins. Each tag-byte callback is checked before and after.
        // Consumers still use the ordinary final buffered-response checks.
        Ok(())
    }

    fn extend_ref_snapshot(
        &mut self,
        credential: &Credential,
        read: &mut ReadSnapshot,
        wanted: &BTreeSet<ObjectId>,
    ) -> Result<()> {
        let new: Vec<_> = wanted
            .iter()
            .filter(|id| !read.objects.contains_key(*id))
            .map(ToString::to_string)
            .collect();
        if new.is_empty() {
            return Ok(());
        }
        if new.len() > self.limits.graph_objects.saturating_sub(read.objects.len()) {
            return Err(Error::Limit("reference snapshot objects"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let repository = &read.scope.repository_instance_id;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if current.scope != read.scope || auth.actor_id != read.actor {
            return Err(Error::Unauthorized);
        }
        if now(&mut tx)? >= read.expires_at {
            return Err(Error::Precondition("read pin expired"));
        }
        let selected = select_objects(&mut tx, repository, &new)?;
        let digests: Vec<_> = selected
            .values()
            .map(|(_, digest, _)| digest.as_str())
            .collect();
        pin_many(
            &mut tx,
            repository,
            &read.pin_id,
            &digests,
            Some(read.expires_at),
        )?;
        let locations = crate::segments::locations(&mut tx, repository, &digests)?;
        // Time can advance while the repository barrier is held.
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let time = now(&mut tx)?;
        if read.expires_at <= time || auth.expires_at <= time {
            return Err(Error::Precondition("read pin or credential expired"));
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        read.objects.extend(selected);
        read.locations.extend(locations);
        Ok(())
    }
}

/// Called only inside the authorized repository transaction. Both initial tips
/// and later tag targets use the same exact published-object membership rule.
pub(super) fn select_objects(
    tx: &mut crate::database::Transaction<'_>,
    repository: &str,
    wanted: &[String],
) -> Result<BTreeMap<ObjectId, (ObjectKind, String, u64)>> {
    let mut selected = BTreeMap::new();
    for batch in wanted.chunks(crate::database::BATCH_ROWS) {
        let rows = tx.query(
            "SELECT o.oid,o.kind,o.blake3,b.bytes FROM unnest($2::text[]) AS wanted(oid) JOIN alt_objects o ON o.repository=$1 AND o.oid=wanted.oid JOIN alt_blobs b ON b.repository=o.repository AND b.blake3=o.blake3 WHERE b.state='stored' AND EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=$1 AND p.blake3=o.blake3 AND p.expires_at IS NULL)",
            &[&repository, &batch],
        )?;
        if rows.len() != batch.len() {
            return Err(Error::Integrity("published reference target unavailable"));
        }
        for row in rows {
            let id: String = row.get(0);
            let kind: String = row.get(1);
            selected.insert(
                id.parse()
                    .map_err(|_| Error::Integrity("reference object id"))?,
                (
                    kind.parse()
                        .map_err(|_| Error::Integrity("reference object kind"))?,
                    row.get::<_, String>(2),
                    unsigned(row.get(3))?,
                ),
            );
        }
    }
    Ok(selected)
}
