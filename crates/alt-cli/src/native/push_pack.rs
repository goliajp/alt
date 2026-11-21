//! The pack a push sends.

use super::*;

/// Git reads a pack behind the commands of every push that creates or
/// updates a ref, and none behind a push that only deletes.
fn sends_pack(updates: &[alt_wire::RefUpdate]) -> bool {
    updates.iter().any(|update| !update.is_delete())
}

impl NativeRepo<'_> {
    /// A plain pack (no deltas) of `outgoing`, as the bytes of the wire
    /// body: a pack of no objects when the server already has everything
    /// `updates` point at, and no bytes at all when they only delete. It
    /// is written into a scratch directory under `<alt-dir>/tmp_push/`
    /// and removed again: nothing keeps it as a stored pack.
    pub(super) fn push_pack(
        &self,
        repo: &alt_repo::Repository,
        remote_name: &str,
        updates: &[alt_wire::RefUpdate],
        outgoing: &[(ObjectId, ObjectKind)],
    ) -> Res<Vec<u8>> {
        if !sends_pack(updates) {
            return Ok(Vec::new());
        }
        let pack_dir = self
            .store
            .alt_dir
            .join("tmp_push")
            .join(format!("push-{remote_name}"));
        std::fs::create_dir_all(&pack_dir)?;
        let count = u32::try_from(outgoing.len()).map_err(|_| "outgoing object set exceeds u32")?;
        let mut writer = alt_git_pack::PackWriter::create(&pack_dir, self.store.algo()?, count)?;
        for (oid, kind) in outgoing {
            let obj = repo
                .read_object(oid)?
                .ok_or_else(|| format!("outgoing object {oid} not in odb"))?;
            writer.add(*oid, *kind, &obj.data)?;
        }
        let written = writer.finish()?;
        let bytes = std::fs::read(&written.pack_path)?;
        // best-effort cleanup; not fatal if the dir is busy
        let _ = crate::durable::remove_dir_all(&pack_dir);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::sends_pack;
    use alt_wire::RefUpdate;

    #[test]
    fn only_a_push_of_nothing_but_deletions_goes_without_a_pack() {
        let oid = "0123456789abcdef0123456789abcdef01234567".parse().ok();
        let update = |old, new| RefUpdate {
            old,
            new,
            name: "refs/heads/topic".into(),
        };
        assert!(sends_pack(&[update(None, oid)]));
        assert!(sends_pack(&[update(oid, oid)]));
        assert!(sends_pack(&[update(oid, None), update(None, oid)]));
        assert!(!sends_pack(&[update(oid, None)]));
        assert!(!sends_pack(&[update(oid, None), update(oid, None)]));
    }
}
