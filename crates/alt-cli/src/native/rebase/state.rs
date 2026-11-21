//! What a rebase that is under way remembers, in `REBASE_STATE` next to
//! the index of the workspace.

use super::super::sequence_editor::Action;
use super::super::*;

/// What a stopped or running rebase remembers.
pub(in crate::native) struct RebaseState {
    pub(super) recovery: Option<String>,
    /// The branch being rebased (`refs/heads/…`).
    pub(in crate::native) branch: String,
    /// Where the branch pointed before the rebase.
    pub(super) orig: ObjectId,
    /// The base the commits go onto.
    pub(in crate::native) onto: ObjectId,
    /// The step that stopped on a conflict, not yet committed.
    pub(in crate::native) current: Option<(Action, ObjectId)>,
    /// Steps still to run, in order.
    pub(in crate::native) todo: Vec<(Action, ObjectId)>,
}

impl RebaseState {
    /// The state file that says this.
    pub(super) fn text(&self) -> String {
        let mut text = format!(
            "branch {}\norig {}\nonto {}\n",
            self.branch, self.orig, self.onto
        );
        if let Some(recovery) = &self.recovery {
            text.push_str(&format!("recovery {recovery}\n"));
        }
        if let Some((a, c)) = self.current {
            text.push_str(&format!("current {} {c}\n", a.word()));
        }
        for (a, c) in &self.todo {
            text.push_str(&format!("todo {} {c}\n", a.word()));
        }
        text
    }
}

impl NativeRepo<'_> {
    fn rebase_state_path(&self) -> PathBuf {
        self.index_path.with_file_name("REBASE_STATE")
    }

    /// Refuses to begin a rebase while another operation waits, or where
    /// the policy forbids force: a rebase ends by putting the branch on
    /// commits that do not descend from the one it is on, and one that
    /// could not end is not begun.
    pub(super) fn ensure_rebase_allowed(&self) -> Res<()> {
        self.ensure_no_merge_in_progress("rebase")?;
        if self.caps.forbid_force {
            let why = "capability denied: forbid_force is set and a rebase would not leave \
                       the branch a fast-forward";
            return Err(crate::machine::Failure::refusal("capability_denied", why).into());
        }
        Ok(())
    }

    pub(in crate::native) fn rebase_in_progress(&self) -> bool {
        self.rebase_state_path().exists()
    }

    pub(in crate::native) fn rebase_state(&self) -> Res<Option<RebaseState>> {
        let text = match std::fs::read_to_string(self.rebase_state_path()) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let corrupt = || "corrupt REBASE_STATE";
        let oid = |v: &str| v.parse::<ObjectId>().map_err(|_| corrupt());
        let (mut branch, mut orig, mut onto, mut current) = (None, None, None, None);
        let mut todo = Vec::new();
        let mut recovery = None;
        for line in text.lines() {
            let (key, value) = line.split_once(' ').ok_or_else(corrupt)?;
            match key {
                "recovery" => recovery = Some(value.to_owned()),
                "branch" => branch = Some(value.to_owned()),
                "orig" => orig = Some(oid(value)?),
                "onto" => onto = Some(oid(value)?),
                "current" => current = Some(parse_step_line(value, &oid)?),
                "todo" => todo.push(parse_step_line(value, &oid)?),
                _ => return Err(corrupt().into()),
            }
        }
        Ok(Some(RebaseState {
            recovery,
            branch: branch.ok_or_else(corrupt)?,
            orig: orig.ok_or_else(corrupt)?,
            onto: onto.ok_or_else(corrupt)?,
            current,
            todo,
        }))
    }

    /// Plans the state file of the rebase: see `plan_state`.
    pub(super) fn save_rebase_state(&self, s: &RebaseState) -> Res<()> {
        self.plan_state("REBASE_STATE", Some(s.text()))
    }

    pub(super) fn clear_rebase_state(&self) -> Res<()> {
        self.plan_state("REBASE_STATE", None)
    }

    /// An `alt commit` during a stopped rebase records that step itself;
    /// `alt rebase --continue` then goes on with the rest.
    pub(in crate::native) fn rebase_committed(&self) -> Res<()> {
        if let Some(mut state) = self.rebase_state()? {
            state.current = None;
            self.save_rebase_state(&state)?;
        }
        Ok(())
    }
}

/// `<action> <oid>` from the state file, trusted as written.
fn parse_step_line(
    value: &str,
    oid: &dyn Fn(&str) -> Result<ObjectId, &'static str>,
) -> Res<(Action, ObjectId)> {
    let (word, c) = value.split_once(' ').ok_or("corrupt REBASE_STATE")?;
    let action = [
        Action::Pick,
        Action::Reword,
        Action::Squash,
        Action::Fixup,
        Action::Drop,
    ]
    .into_iter()
    .find(|a| a.word() == word)
    .ok_or("corrupt REBASE_STATE")?;
    Ok((action, oid(c)?))
}
