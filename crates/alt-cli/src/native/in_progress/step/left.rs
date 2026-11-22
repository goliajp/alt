//! A step that was cut short and another program wrote after: left as it
//! is, its mark closed with a record that tells nothing.

use super::super::super::*;
use super::recover::Told;
use super::{STATE_FILES, StepRecord};

impl NativeRepo<'_> {
    /// Closes the mark `begun` of a step another program wrote after,
    /// with a record that tells nothing. Where a state file is not as the
    /// log says, only when `asked` to: `alt operation recover` is.
    pub(super) fn leave_as_it_is(
        &mut self,
        begun: &alt_oplog::Op,
        told: &Told,
        before: &[Option<String>],
        mut step: StepRecord,
        asked: bool,
    ) -> Res<()> {
        let id = begun.id;
        let shown = |text: &Option<String>| match text {
            Some(text) => format!("holds {text:?}"),
            None => "is not there".to_owned(),
        };
        let (mut differ, mut how) = (Vec::new(), Vec::new());
        for (name, before) in STATE_FILES.iter().zip(before) {
            let (found, said) = (self.state_file(name)?, told.state.get(*name));
            let said = said.unwrap_or(before);
            if found != *said {
                differ.push(*name);
                let (found, said) = (shown(&found), shown(said));
                how.push(format!("{name} {found}, and the log says it {said}"));
            }
        }
        let (differ, how) = (differ.join(", "), how.join("; "));
        if !differ.is_empty() && !asked {
            let said = format!(
                "operation {id} was interrupted and another program has written since: \
                 {differ} is not as the operation log says ({how}), so what `alt status` shows \
                 of an operation under way may be wrong; put it as the log says, or accept it \
                 as it is with `alt operation recover {id}`: that closes the operation and \
                 changes no file, and from then on what the file says holds, for `alt status` \
                 and for the next commit, `--continue` or `--abort`"
            );
            return Err(crate::machine::Failure::pending(id, said).into());
        }
        let said = match differ.is_empty() {
            true => String::new(),
            false => format!(" ({differ} is not as the operation log says)"),
        };
        let said = format!(
            "warning: operation {id} was interrupted and another program has written since: \
             it is left as it is{said}; see `alt status`\n"
        );
        crate::diagnostics::emit(said.as_bytes())?;
        step.abandoned = true;
        if let Some(guard) = told.guard {
            self.close_guard(guard)?;
        }
        self.record_step("recover", &step)
    }
}
