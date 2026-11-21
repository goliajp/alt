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
//! The commit that concludes the pick removes it, and so does
//! `alt cherry-pick --abort`. An alt from before the
//! file existed commits without removing it, so it only counts while HEAD
//! is where the file says; once HEAD has moved it is stale, and the next
//! command that writes removes it.

use super::super::*;

impl NativeRepo<'_> {
    fn cherry_pick_path(&self) -> PathBuf {
        self.index_path.with_file_name("CHERRY_PICK_HEAD")
    }

    /// Records that picking `picked` onto `head` stopped, with the
    /// conflict the pick is about to write.
    pub(in crate::native) fn start_cherry_pick(&self, picked: ObjectId, head: ObjectId) -> Res<()> {
        self.plan_state(
            "CHERRY_PICK_HEAD",
            Some(format!("picked {picked}\nhead {head}\n")),
        )
    }

    /// The commit being picked and the commit HEAD was at when a
    /// cherry-pick stopped, if the file of one is there.
    fn cherry_pick_file(&self) -> Res<Option<(ObjectId, ObjectId)>> {
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
        Ok(Some((oid("picked ")?, oid("head ")?)))
    }

    /// The cherry-pick that stopped here and waits for its commit: the
    /// commit being picked and the commit HEAD is at.
    pub(super) fn stopped_cherry_pick(&self) -> Res<Option<(ObjectId, ObjectId)>> {
        let Some((picked, base)) = self.cherry_pick_file()? else {
            return Ok(None);
        };
        let at_base = self.store.refs.resolve(&self.head_ref)? == Some(base);
        Ok(at_base.then_some((picked, base)))
    }

    /// Whether a cherry-pick stopped here and waits for its commit.
    pub(in crate::native) fn cherry_pick_in_progress(&self) -> Res<bool> {
        Ok(self.stopped_cherry_pick()?.is_some())
    }

    pub(super) fn clear_cherry_pick(&self) -> Res<()> {
        self.plan_state("CHERRY_PICK_HEAD", None)
    }

    /// Refuses a merge while a cherry-pick waits for its commit, as git does.
    pub fn ensure_no_pick_to_conclude(&self) -> Res<()> {
        if self.cherry_pick_in_progress()? {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "merge: a cherry-pick is in progress; conclude it with `alt cherry-pick \
                 --continue` or give it up with `alt cherry-pick --abort` first",
            )
            .into());
        }
        Ok(())
    }

    /// Before a command that writes: removes the file of a cherry-pick
    /// that HEAD has moved away from.
    pub fn forget_stale_cherry_pick(&self) -> Res<()> {
        if self.cherry_pick_file()?.is_some() && !self.cherry_pick_in_progress()? {
            self.clear_cherry_pick()?;
        }
        Ok(())
    }

    /// Before `alt commit` records its commit: what the commit concludes
    /// of a sequence that stopped for it, to hold with the commit, and the
    /// author of the commit being picked, whom it keeps.
    pub(in crate::native) fn concluding(&self) -> Res<Option<Sig<'static>>> {
        let picked = self.picked_author()?;
        self.finish_merge()?;
        self.revert_committed()?;
        self.rebase_committed()?;
        self.clear_cherry_pick()?;
        Ok(picked)
    }
}
