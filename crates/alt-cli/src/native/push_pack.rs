//! The pack a push sends.

use super::*;

impl NativeRepo<'_> {
    /// A plain pack (no deltas) of `outgoing`, as the bytes of the wire
    /// body. It is written into a scratch directory under
    /// `<alt-dir>/tmp_push/` and removed again: nothing keeps it as a
    /// stored pack.
    pub(super) fn push_pack(
        &self,
        repo: &alt_repo::Repository,
        remote_name: &str,
        outgoing: &[(ObjectId, ObjectKind)],
    ) -> Res<Vec<u8>> {
        if outgoing.is_empty() {
            // a push that only deletes, or changes nothing: servers accept
            // an empty body when no command ships a pack
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
