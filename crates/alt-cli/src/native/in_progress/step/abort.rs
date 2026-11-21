//! What an `--abort` does about a step that was cut short, before it
//! gives its operation up.

use super::super::super::*;
use super::STATE_FILES;

/// The operation the state file `state` says is under way.
fn operation_of(state: &str) -> &'static str {
    match state {
        "MERGE_HEAD" => "merge",
        "CHERRY_PICK_HEAD" => "cherry-pick",
        "REVERT_STATE" => "revert",
        _ => "rebase",
    }
}

impl NativeRepo<'_> {
    /// Before an `--abort`: a step that was cut short is brought to an
    /// end, or given up where it cannot be and is a step of the operation
    /// the abort is of, the one that has the state file `state`. A step
    /// of another operation is not given up for it: the abort is refused
    /// and says what waits. Whether the step was given up and had made
    /// `state`, so that the operation is given up with it.
    pub fn begin_abort(&mut self, state: &str) -> Res<bool> {
        self.ensure_may_recover()?;
        let Err(stuck) = self.recover_open_step(false) else {
            return Ok(false);
        };
        let Some((begun, mark, false)) = self.last_step_mark()? else {
            return Err(stuck);
        };
        let told = self.told_after(&begun, &mark)?;
        let found = |name: &str| {
            mark.before
                .iter()
                .any(|f| f.name == name && f.text.is_some())
        };
        let made = |name: &str| told.state.get(name).is_some_and(Option::is_some);
        if !found(state) && !made(state) {
            let of = STATE_FILES.iter().find(|name| found(name) || made(name));
            let Some(of) = of.copied().map(operation_of) else {
                return Err(stuck);
            };
            let asked = operation_of(state);
            let why = format!(
                "{stuck}. It is a command of a {of}, which is not given up for the abort of a \
                 {asked}: `alt {of} --abort` gives it up"
            );
            return Err(crate::machine::Failure::refusal("precondition_failed", why).into());
        }
        let (made, _) = self.abandon_open_step(false)?;
        Ok(made.iter().any(|name| name == state))
    }
}
