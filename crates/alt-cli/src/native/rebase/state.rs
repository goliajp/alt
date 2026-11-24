//! What a rebase that is under way remembers, in `REBASE_STATE` next to
//! the index of the workspace.

use super::super::sequence_editor::Action;
use super::super::*;

pub(in crate::native) use super::super::sequence_editor::Step;

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
    pub(in crate::native) todo: Vec<Step>,
    /// Each commit replayed is authored at the moment it is replayed.
    pub(super) fresh_dates: bool,
    /// It stopped for a shell command to be run, and has not gone on.
    pub(super) ran: bool,
    /// Where the rebase stood at each label set so far.
    pub(super) labels: Vec<(String, ObjectId)>,
    /// The merge commit being made again that stopped on a conflict,
    /// and the commit it brings in.
    pub(in crate::native) merging: Option<(ObjectId, ObjectId)>,
    /// The step that stopped and was concluded by a commit of the user:
    /// what is staged when the rebase goes on is committed as it.
    pub(in crate::native) concluded: Option<(Action, ObjectId)>,
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
        if let Some((a, c)) = self.concluded {
            text.push_str(&format!("done {} {c}\n", a.word()));
        }
        if self.fresh_dates {
            text.push_str("dates fresh\n");
        }
        if self.ran {
            text.push_str("ran exec\n");
        }
        for (name, at) in &self.labels {
            text.push_str(&format!("label {at} {name}\n"));
        }
        if let Some((commit, brought)) = self.merging {
            text.push_str(&format!("merging {commit} {brought}\n"));
        }
        for step in &self.todo {
            text.push_str(&match step {
                Step::Replay(a, c) => format!("todo {} {c}\n", a.word()),
                Step::Run(command) => format!("todo exec {command}\n"),
                Step::Label(name) => format!("todo label {name}\n"),
                Step::Reset(to) => format!("todo reset {to}\n"),
                Step::Merge(c, brought) => format!("todo merge {c} {}\n", brought.join(" ")),
            });
        }
        text
    }

    /// Whether it stopped on a conflict that is not committed yet.
    pub(in crate::native) fn stopped(&self) -> bool {
        self.current.is_some() || self.merging.is_some()
    }

    /// How many commits are still to replay.
    pub(in crate::native) fn replays_left(&self) -> usize {
        let replays = self.todo.iter();
        replays
            .filter(|s| matches!(s, Step::Replay(..) | Step::Merge(..)))
            .count()
    }
}

impl NativeRepo<'_> {
    fn rebase_state_path(&self) -> PathBuf {
        self.index_path.with_file_name("REBASE_STATE")
    }

    /// Refuses, before it begins, a rebase that the policy would not let
    /// end where it forbids force: one that replays a commit, or drops
    /// every one of them onto a base that does not descend from the
    /// branch, ends by putting the branch on commits that do not descend
    /// from the one it is on.
    pub(super) fn ensure_rebase_may_end(
        &self,
        orig: ObjectId,
        onto: ObjectId,
        todo: &[Step],
    ) -> Res<()> {
        let replays = todo.iter().any(|step| step.made().is_some());
        if self.caps.forbid_force && (replays || !self.is_ancestor(orig, onto)?) {
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
        let (mut recovery, mut fresh_dates, mut ran) = (None, false, false);
        let (mut labels, mut merging, mut concluded) = (Vec::new(), None, None);
        for line in text.lines() {
            let (key, value) = line.split_once(' ').ok_or_else(corrupt)?;
            match key {
                "recovery" => recovery = Some(value.to_owned()),
                "branch" => branch = Some(value.to_owned()),
                "orig" => orig = Some(oid(value)?),
                "onto" => onto = Some(oid(value)?),
                "current" => current = Some(parse_step_line(value, &oid)?),
                "done" => concluded = Some(parse_step_line(value, &oid)?),
                "dates" => fresh_dates = true,
                "ran" => ran = true,
                "label" => {
                    let (at, name) = value.split_once(' ').ok_or_else(corrupt)?;
                    labels.push((name.to_owned(), oid(at)?));
                }
                "merging" => {
                    let (commit, brought) = value.split_once(' ').ok_or_else(corrupt)?;
                    merging = Some((oid(commit)?, oid(brought)?));
                }
                "todo" => todo.push(parse_todo_line(value, &oid)?),
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
            fresh_dates,
            ran,
            labels,
            merging,
            concluded,
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
            state.concluded = state.current.take().or(state.concluded);
            self.save_rebase_state(&state)?;
        }
        Ok(())
    }
}

/// A step still to run from the state file, trusted as written.
fn parse_todo_line(value: &str, oid: &dyn Fn(&str) -> Result<ObjectId, &'static str>) -> Res<Step> {
    let (word, rest) = value.split_once(' ').ok_or("corrupt REBASE_STATE")?;
    Ok(match word {
        "exec" => Step::Run(rest.to_owned()),
        "label" => Step::Label(rest.to_owned()),
        "reset" => Step::Reset(rest.to_owned()),
        "merge" => {
            let (commit, brought) = rest.split_once(' ').ok_or("corrupt REBASE_STATE")?;
            Step::Merge(
                oid(commit)?,
                brought.split(' ').map(str::to_owned).collect(),
            )
        }
        _ => {
            let (action, commit) = parse_step_line(value, oid)?;
            Step::Replay(action, commit)
        }
    })
}

impl Step {
    /// The commit this step makes anew, where it makes one.
    pub(super) fn made(&self) -> Option<ObjectId> {
        match self {
            Step::Replay(action, commit) if *action != Action::Drop => Some(*commit),
            Step::Merge(commit, _) => Some(*commit),
            _ => None,
        }
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
