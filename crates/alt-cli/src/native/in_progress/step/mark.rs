//! The mark a step begins with, and what the log says of the state files
//! before any of them changes.
//!
//! The first mark of a step tells each state file as the step found it.
//! That mark and any after it tell what state files become, in one of two
//! ways. With the next record: a ref transaction or the intent of a
//! checkout that changes together with them, so that what the mark tells
//! holds if that record is the next of the step in the log, and does not
//! if the step was cut short between the two. Or at once, where nothing
//! else changes with them. The file itself is written or removed only
//! after the log, synced, says so.

use super::super::super::recovery::{WorkspaceIdentity, durable_write, encode};
use super::super::super::*;
use super::{STATE_FILES, Tracked};

pub(in crate::native) const STEP_MARK: u8 = 0x37;

/// What a state file holds; nothing where there is no such file.
#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct StateText {
    pub(super) name: String,
    pub(super) text: Option<String>,
}

#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct StepMark {
    pub(super) version: u32,
    pub(super) workspace: WorkspaceIdentity,
    #[json(default)]
    pub(super) request_id: Option<String>,
    /// the mark the step began with; none in that mark itself
    pub(super) begin: Option<String>,
    /// the step an undo takes back, in the mark it begins with
    pub(super) inverse_of: Option<String>,
    /// each state file as the step found it, in the mark it begins with
    pub(super) before: Vec<StateText>,
    /// what state files become
    pub(super) state: Vec<StateText>,
    /// only once the next record of the step is there
    pub(super) with_effect: bool,
}

impl Tracked {
    /// A step that takes the step `inverse_of` back.
    pub(super) fn undoing(before: Vec<Option<String>>, inverse_of: String) -> Self {
        Tracked {
            before,
            eager: true,
            inverse_of: Some(inverse_of),
            ..Default::default()
        }
    }

    /// What the state file `name` holds once all that is planned holds.
    fn planned_text(&self, name: &str) -> &Option<String> {
        let planned = self.said.iter().chain(&self.planned);
        if let Some(planned) = planned.rev().find(|p| p.name == name) {
            return &planned.text;
        }
        let at = STATE_FILES.iter().position(|known| *known == name);
        let before = &self.before[at.expect("a state file")];
        self.now.get(name).unwrap_or(before)
    }

    fn holds(&mut self, state: Vec<StateText>) {
        for file in state {
            self.unapplied.retain(|name| *name != file.name);
            self.unapplied.push(file.name.clone());
            self.now.insert(file.name, file.text);
        }
    }
}

impl NativeRepo<'_> {
    pub(super) fn set_state_file(&self, name: &str, text: &Option<String>) -> Res<()> {
        crash_point!("state-file:before");
        match text {
            Some(text) => durable_write(&self.state_path(name), text.as_bytes())?,
            None => match crate::durable::remove_file(self.state_path(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            },
        }
        crash_point!("state-file:after");
        Ok(())
    }

    /// What the state file `name` becomes with the next ref transaction
    /// or checkout of the command, or by [`settle_state`](Self::settle_state)
    /// where none comes. A command that is not kept track of writes the
    /// file at once.
    pub(in crate::native) fn plan_state(&self, name: &str, text: Option<String>) -> Res<()> {
        let mut step = self.step.borrow_mut();
        let Some(tracked) = step.as_mut() else {
            return self.set_state_file(name, &text);
        };
        if *tracked.planned_text(name) != text {
            tracked.planned.retain(|planned| planned.name != name);
            let name = name.to_owned();
            tracked.planned.push(StateText { name, text });
        }
        Ok(())
    }

    fn write_mark(&mut self, state: Vec<StateText>, with_effect: bool) -> Res<()> {
        let (begin, inverse_of, before) = {
            let step = self.step.borrow();
            let tracked = step.as_ref().expect("a command kept track of");
            let before = STATE_FILES.iter().zip(&tracked.before);
            let before = before.map(|(name, text)| StateText {
                name: (*name).to_owned(),
                text: text.clone(),
            });
            match tracked.begin {
                Some(begin) => (Some(begin.to_string()), None, Vec::new()),
                None => (None, tracked.inverse_of.clone(), before.collect()),
            }
        };
        let first = begin.is_none();
        let mark = StepMark {
            version: 1,
            workspace: self.workspace_identity()?,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            begin,
            inverse_of,
            before,
            state,
            with_effect,
        };
        let actor = self.id.actor("step");
        let payload = encode(STEP_MARK, &mark)?;
        crash_point!("step-mark:before");
        // durable with the record that comes next, which is synced
        let id = self
            .store
            .refs
            .record_op_unsynced(&actor, now_ms(), &payload)?;
        crash_point!("step-mark:after");
        if first {
            self.step.borrow_mut().as_mut().expect("as above").begin = Some(id);
        }
        Ok(())
    }

    /// Before a ref transaction or the intent of a checkout is written to
    /// the log: the mark that begins the step, where it is one and has
    /// none yet, with what is planned to hold with that record.
    pub(in crate::native) fn step_effect(&mut self) -> Res<()> {
        let state = match self.step.borrow_mut().as_mut() {
            Some(tracked) if !tracked.planned.is_empty() => std::mem::take(&mut tracked.planned),
            Some(tracked) if tracked.eager && tracked.begin.is_none() => Vec::new(),
            _ => return Ok(()),
        };
        self.write_mark(state.clone(), true)?;
        let mut step = self.step.borrow_mut();
        step.as_mut().expect("as above").said.extend(state);
        Ok(())
    }

    /// After that record is in the log: what was planned with it holds.
    pub(in crate::native) fn step_effected(&self) {
        if let Some(tracked) = self.step.borrow_mut().as_mut() {
            let said = std::mem::take(&mut tracked.said);
            tracked.holds(said);
        }
    }

    /// What is still planned holds from here on: no record came for it to
    /// hold with, as where a checkout had nothing to change.
    pub(in crate::native) fn settle_state(&mut self) -> Res<()> {
        let state = match self.step.borrow_mut().as_mut() {
            Some(tracked) if !tracked.planned.is_empty() => std::mem::take(&mut tracked.planned),
            _ => return Ok(()),
        };
        self.write_mark(state.clone(), false)?;
        let mut step = self.step.borrow_mut();
        step.as_mut().expect("as above").holds(state);
        Ok(())
    }

    /// Brings the state files to what the log says they have become.
    /// `sync` where the log is not known to be synced that far.
    pub(in crate::native) fn apply_state(&mut self, sync: bool) -> Res<()> {
        let unapplied: Vec<(String, Option<String>)> = match self.step.borrow_mut().as_mut() {
            Some(tracked) => {
                let names = std::mem::take(&mut tracked.unapplied).into_iter();
                names
                    .map(|n| (n.clone(), tracked.now[&n].clone()))
                    .collect()
            }
            None => return Ok(()),
        };
        if sync && !unapplied.is_empty() {
            self.store.sink()?.fsync_all()?;
        }
        for (name, text) in &unapplied {
            self.set_state_file(name, text)?;
        }
        Ok(())
    }
}
