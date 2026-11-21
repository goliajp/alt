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
        let mut differ = Vec::new();
        for (name, before) in STATE_FILES.iter().zip(before) {
            if self.state_file(name)? != *told.state.get(*name).unwrap_or(before) {
                differ.push(*name);
            }
        }
        let differ = differ.join(", ");
        if !differ.is_empty() && !asked {
            let said = format!(
                "operation {id} was interrupted and another program has written since: \
                 {differ} is not as the operation log says, so what `alt status` shows of an \
                 operation under way may be wrong; put it right or accept it as it is, then \
                 close the operation with `alt operation recover {id}`"
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
