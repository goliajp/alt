//! What a cherry-pick that stopped on a conflict leaves behind.
//!
//! `CHERRY_PICK_HEAD` lies next to the index of the workspace and names
//! the commit being picked and the commit HEAD was at:
//!
//! ```text
//! picked <oid>
//! head <oid>
//! ```
//!
//! The commit that concludes the pick removes it. An alt from before the
//! file existed commits without removing it, so it only counts while HEAD
//! is where the file says; once HEAD has moved it is stale, and the next
//! command that writes removes it.

use super::super::*;

impl NativeRepo<'_> {
    fn cherry_pick_path(&self) -> PathBuf {
        self.index_path.with_file_name("CHERRY_PICK_HEAD")
    }

    /// Records that picking `picked` onto `head` stopped.
    pub(in crate::native) fn start_cherry_pick(&self, picked: ObjectId, head: ObjectId) -> Res<()> {
        let text = format!("picked {picked}\nhead {head}\n");
        crate::durable::write(self.cherry_pick_path(), text)?;
        Ok(())
    }

    /// The commit HEAD was at when a cherry-pick stopped, if the file of
    /// one is there.
    fn cherry_pick_base(&self) -> Res<Option<ObjectId>> {
        let text = match std::fs::read_to_string(self.cherry_pick_path()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let corrupt = || "corrupt CHERRY_PICK_HEAD";
        let mut lines = text.lines();
        let mut oid = |key: &str| -> Res<ObjectId> {
            let line = lines.next().ok_or_else(corrupt)?;
            let value = line.strip_prefix(key).ok_or_else(corrupt)?;
            Ok(value.parse().map_err(|_| corrupt())?)
        };
        oid("picked ")?;
        Ok(Some(oid("head ")?))
    }

    /// Whether a cherry-pick stopped here and waits for its commit.
    pub(in crate::native) fn cherry_pick_in_progress(&self) -> Res<bool> {
        let Some(base) = self.cherry_pick_base()? else {
            return Ok(false);
        };
        Ok(self.store.refs.resolve(&self.head_ref)? == Some(base))
    }

    fn clear_cherry_pick(&self) -> Res<()> {
        match crate::durable::remove_file(self.cherry_pick_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Before a command that writes: removes the file of a cherry-pick
    /// that HEAD has moved away from.
    pub fn forget_stale_cherry_pick(&self) -> Res<()> {
        if self.cherry_pick_base()?.is_some() && !self.cherry_pick_in_progress()? {
            self.clear_cherry_pick()?;
        }
        Ok(())
    }

    /// After `alt commit`: what a commit concludes of a sequence that
    /// stopped for it.
    pub(in crate::native) fn sequence_committed(&self) -> Res<()> {
        self.revert_committed()?;
        self.rebase_committed()?;
        self.clear_cherry_pick()
    }
}
