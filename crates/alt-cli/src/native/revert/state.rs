//! What a revert sequence that stopped remembers, in `REVERT_STATE` next
//! to the index of the workspace.

use super::super::*;

/// A state file line by line, before `orig` is known to be present.
struct RevertStateParts {
    current: Option<ObjectId>,
    todo: Vec<ObjectId>,
    mainline: Option<usize>,
    orig: Option<ObjectId>,
}

/// What a stopped revert sequence remembers.
pub(in crate::native) struct RevertState {
    /// The commit whose revert stopped on a conflict; `None` once that revert
    /// has been committed (by `alt commit`) and only the rest is left.
    pub(in crate::native) current: Option<ObjectId>,
    /// Commits still to revert, in order.
    pub(in crate::native) todo: Vec<ObjectId>,
    /// The parent to revert against, for merge commits (1-based, as `-m`).
    pub(super) mainline: Option<usize>,
    /// Where the branch stood before the sequence, for `--abort`.
    pub(in crate::native) orig: ObjectId,
}

impl NativeRepo<'_> {
    fn revert_state_path(&self) -> PathBuf {
        self.index_path.with_file_name("REVERT_STATE")
    }

    /// The stopped sequence, if one is waiting.
    pub(in crate::native) fn revert_state(&self) -> Res<Option<RevertState>> {
        let text = match std::fs::read_to_string(self.revert_state_path()) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let corrupt = || "corrupt REVERT_STATE";
        let mut state = RevertStateParts {
            current: None,
            todo: Vec::new(),
            mainline: None,
            orig: None,
        };
        for line in text.lines() {
            let (key, value) = line.split_once(' ').ok_or_else(corrupt)?;
            match key {
                "current" => state.current = Some(value.parse().map_err(|_| corrupt())?),
                "todo" => state.todo.push(value.parse().map_err(|_| corrupt())?),
                "mainline" => state.mainline = Some(value.parse().map_err(|_| corrupt())?),
                "orig" => state.orig = Some(value.parse().map_err(|_| corrupt())?),
                _ => return Err(corrupt().into()),
            }
        }
        Ok(Some(RevertState {
            current: state.current,
            todo: state.todo,
            mainline: state.mainline,
            orig: state.orig.ok_or_else(corrupt)?,
        }))
    }

    /// Plans the state file of the sequence: see `plan_state`.
    pub(super) fn save_revert_state(&self, s: &RevertState) -> Res<()> {
        let mut text = format!("orig {}\n", s.orig);
        if let Some(c) = s.current {
            text.push_str(&format!("current {c}\n"));
        }
        if let Some(m) = s.mainline {
            text.push_str(&format!("mainline {m}\n"));
        }
        for t in &s.todo {
            text.push_str(&format!("todo {t}\n"));
        }
        self.plan_state("REVERT_STATE", Some(text))
    }

    /// What is left of a sequence once one of its reverts is committed:
    /// nothing where none waits.
    pub(super) fn save_revert_rest(&self, rest: &RevertState) -> Res<()> {
        match rest.todo.is_empty() {
            true => self.clear_revert_state(),
            false => self.save_revert_state(rest),
        }
    }

    pub(super) fn clear_revert_state(&self) -> Res<()> {
        match std::fs::remove_file(self.revert_refused_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        self.plan_state("REVERT_STATE", None)
    }

    fn revert_refused_path(&self) -> PathBuf {
        self.index_path.with_file_name("REVERT_REFUSED")
    }

    /// Keeps that the sequence refused the revert that waits first, with
    /// the branch at `head`. It is kept apart from the state, which an
    /// older alt reads and takes no line it does not know in.
    pub(super) fn note_revert_refused(&self, head: ObjectId) -> Res<()> {
        let noted = format!("{head}\n");
        Ok(recovery::durable_write(
            &self.revert_refused_path(),
            noted.as_bytes(),
        )?)
    }

    /// Whether the revert that waits first is one the sequence refused,
    /// and the branch has not moved since: git then skips that one, and
    /// gives up from where the sequence began.
    pub(super) fn revert_refused_here(&self) -> Res<bool> {
        let noted = match std::fs::read_to_string(self.revert_refused_path()) {
            Ok(noted) => noted,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        Ok(noted.trim() == self.branch_tip("revert")?.to_string())
    }

    /// An `alt commit` in the middle of a stopped revert concludes that one
    /// revert; the rest of the sequence waits for `alt revert --continue`.
    pub(in crate::native) fn revert_committed(&self) -> Res<()> {
        if let Some(mut state) = self.revert_state()? {
            state.current = None;
            self.save_revert_state(&state)?;
        }
        Ok(())
    }
}
