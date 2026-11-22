//! The index a merge that stopped on conflicts left. No operation records
//! it, so it is kept as a recovery snapshot and noted beside the index
//! with the last operation of the log at that time, which is synced
//! first: an index file that is torn later is put back to the conflicts,
//! not to what was there before the merge.
//!
//! A build older than this note leaves none. The first add of this build
//! that finds a stop without a note that holds keeps the index as it
//! finds it, synced, before it changes it. Where a stop is under way and
//! what can be put back for a torn index is not the index the stop left
//! or one made from it, a commit from it would leave out what the stop
//! staged: commands that write are refused, but for an add of the whole
//! working tree, which stages the outcome again. Not a reset to a commit
//! either: it writes the files that differ from the index, and by an
//! index from before the stop it would leave what the stop wrote.

use super::*;

/// The index a merge that stopped left, kept beside the index: no
/// operation records it.
#[derive(alt_json::ToJson, alt_json::FromJson)]
pub(super) struct Stopped {
    version: u32,
    /// the snapshot of the index
    pub(super) index: String,
    /// the last operation of the log when it was written
    pub(super) after: String,
}

fn stopped_path(index_path: &Path) -> PathBuf {
    index_path.with_file_name("index.stopped")
}

pub(super) fn stopped_at(index_path: &Path) -> Option<Stopped> {
    let bytes = std::fs::read(stopped_path(index_path)).ok()?;
    let stopped: Stopped = alt_json::from_slice(&bytes).ok()?;
    (stopped.version == 1).then_some(stopped)
}

/// What a command that writes is told where the index a stop left is
/// lost.
pub(super) fn refusal(stop: &str) -> Box<dyn std::error::Error> {
    let out = match stop {
        "rebase" => "; after that the rebase can also be given up with `alt rebase --abort`",
        "revert" => "; after that the revert can also be given up with `alt revert --abort`",
        _ => "",
    };
    let said = format!(
        "the index file of this workspace is not a whole index (a power cut leaves it so when \
         the last `alt add` was not synced), and the index the {stop} that stopped on conflicts \
         left is lost with it. What can be put back is from before the {stop} stopped and holds \
         nothing of what it staged: a commit made from it would leave that out. Stage the \
         working tree as a whole again with `alt add -A` and look at `alt status` before going \
         on{out}"
    );
    crate::machine::Failure::refusal("precondition_failed", said).into()
}

impl NativeRepo<'_> {
    /// What has stopped on conflicts in this workspace, by the files it
    /// leaves beside the index.
    fn marked_stopped(&self) -> Res<Option<&'static str>> {
        if self.index_path.with_file_name("MERGE_HEAD").exists() {
            return Ok(Some("merge"));
        }
        // a sequence names the step it stopped at
        for (file, stop) in [("REVERT_STATE", "revert"), ("REBASE_STATE", "rebase")] {
            match std::fs::read_to_string(self.index_path.with_file_name(file)) {
                Ok(state) if state.lines().any(|line| line.starts_with("current ")) => {
                    return Ok(Some(stop));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(None)
    }

    /// The operations of the log since the stop the note beside the index
    /// tells of; `None` where there is no note, or the workspace has moved
    /// a ref or checked a tree out since: the note is of an earlier stop.
    fn since_stop(&self) -> Res<Option<Vec<OpId>>> {
        let Some(stopped) = stopped_at(&self.index_path) else {
            return Ok(None);
        };
        let Ok(instance) = std::fs::read_to_string(self.index_path.with_file_name("workspace-id"))
        else {
            return Ok(None);
        };
        let identity = WorkspaceIdentity {
            name: self.workspace.clone(),
            instance,
        };
        let (mut since, mut later) = (Vec::new(), Later::default());
        for op in self.store.refs.oplog().ops()?.iter().rev() {
            if stopped.after == op.id.to_string() {
                return Ok(Some(since));
            }
            match self.left_by(op, &identity, &mut later)? {
                Left::Nothing => {}
                Left::Refs | Left::Index(_, true) => return Ok(None),
                Left::Index(_, false) if op.payload.first() == Some(&PREPARE) => since.push(op.id),
                // a checkout, or what a step that was given up found
                Left::Index(_, false) => return Ok(None),
            }
        }
        Ok(None)
    }

    /// What had stopped on conflicts, where an index from `from` is not
    /// the one the stop left or one made from it.
    pub(super) fn lost_stop(&self, from: &Source) -> Res<Option<&'static str>> {
        let since = self.since_stop()?;
        let stop = match (self.marked_stopped()?, &since) {
            (Some(stop), _) => stop,
            // a cherry-pick leaves no file beside the index
            (None, Some(_)) => "cherry-pick",
            (None, None) => return Ok(None),
        };
        let kept = match (from, since) {
            (Source::StoppedMerge, Some(_)) => true,
            (Source::Operation(id), Some(since)) => since.contains(id),
            _ => false,
        };
        Ok((!kept).then_some(stop))
    }

    /// What an add does before it reads anything: the workspace is
    /// locked, nothing is left open in it, and the index is read. An add
    /// of the whole working tree (`update`: of tracked files only) stages
    /// the outcome of a stop again and needs nothing of the index the
    /// stop left, should a power cut have taken it.
    pub(in crate::native) fn begin_add(
        &mut self,
        paths: &[String],
        update: bool,
    ) -> Res<recovery::AsRead> {
        let whole = |path: &String| path == ":/" || (path == "." && self.prefix.is_empty());
        self.torn
            .lost_stop_ok
            .set(!update && paths.iter().any(whole));
        self.ensure_writable("add")?;
        self.lock_workspace()?;
        self.ensure_no_pending_recovery()?;
        let before = self.index_as_read()?;
        self.keep_unnoted_stop(&before)?;
        Ok(before)
    }

    /// After an add put `index` in place of one from before a stop: the
    /// outcome of the stop as it is staged now is the index the stop
    /// left from here on.
    pub(in crate::native) fn keep_restaged(&mut self, index: Option<Index>) -> Res<()> {
        let Some(index) = index else {
            return Ok(());
        };
        let _synced = alt_os::durability::leave_unsynced(false);
        self.keep_stopped(index)
    }

    /// Before an add changes `index`: where something has stopped on
    /// conflicts and no note that holds tells of the index it left (an
    /// older build made the stop), `index` is kept as that index, synced
    /// whatever the add syncs.
    fn keep_unnoted_stop(&mut self, index: &Index) -> Res<()> {
        if self.lost_stop_restored() || self.marked_stopped()?.is_none() {
            return Ok(());
        }
        if self.since_stop()?.is_some() {
            return Ok(());
        }
        let _synced = alt_os::durability::leave_unsynced(false);
        self.keep_stopped(index.clone())
    }

    /// Whether this command put an index from before a stop in place of
    /// the lost one the stop left.
    pub(in crate::native) fn lost_stop_restored(&self) -> bool {
        let restored = self.torn.restored.borrow();
        restored.as_ref().is_some_and(|r| r.lost_stop.is_some())
    }

    /// Keeps `index`, which a merge that stopped has just put in place,
    /// where it is found again should the index file be torn later: no
    /// operation records this index.
    pub(in crate::native) fn keep_stopped(&mut self, index: Index) -> Res<()> {
        // the note names the last operation of the log: one an add left
        // unsynced can go with a power cut, and the note would then name
        // nothing
        self.store.refs.bound()?;
        let Some(after) = self.store.refs.head_op() else {
            return Ok(());
        };
        let stopped = Stopped {
            version: 1,
            index: self.snapshot(&canonical_index(index, self.store.algo()?))?,
            after: after.to_string(),
        };
        let bytes = alt_json::to_vec(&stopped)?;
        Ok(durable_write(&stopped_path(&self.index_path), &bytes)?)
    }
}
