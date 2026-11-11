//! Bounded reference/tag projection. Ordinary fetch/export snapshots remain full.
use super::*;
use alt_repository_service::{Node, verify_object};

impl Store {
    /// Select only matching ref tips and their annotated-tag chains. Commit/tree
    /// descendants are not selected: this snapshot is for listing refs, not
    /// fetch or export. Tag originals are verified outside database locks.
    /// Callers must still buffer output with `with_snapshot_objects`.
    pub fn read_ref_snapshot(
        &mut self,
        credential: &Credential,
        repository: &str,
        ttl_ms: u64,
        prefixes: &[String],
    ) -> Result<ReadSnapshot> {
        let mut read =
            self.read_snapshot_inner(credential, repository, ttl_ms, Selection::RefTips(prefixes))?;
        self.select_ref_targets(credential, &mut read)?;
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
                    .checked_sub(read.objects[id].1)
                    .ok_or(Error::Limit("reference tag bytes"))?;
            }
            // authority and expiry are checked before and after each round of tag reads
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
            let new: BTreeSet<_> = targets
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| !read.objects.contains_key(id))
                .collect();
            if new.len() > self.limits.graph_objects.saturating_sub(read.objects.len()) {
                return Err(Error::Limit("reference snapshot objects"));
            }
            read.select(new)?;
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
        Ok(())
    }
}
