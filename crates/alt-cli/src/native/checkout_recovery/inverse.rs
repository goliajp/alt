//! The checkout that takes a recorded one back.

use super::super::*;
use super::{CheckoutIntent, FileChange};
use recovery::{canonical_index, hash};

impl NativeRepo<'_> {
    /// Invert the recorded workspace delta, after validating its entire
    /// poststate. Resolve no files from the new or old HEAD trees.
    pub(in crate::native) fn inverse_checkout(
        &self,
        intent: &CheckoutIntent,
    ) -> Res<CheckoutIntent> {
        if intent.version != 1 || intent.workspace != self.workspace_identity()? {
            return Err(crate::machine::Failure::refusal(
                "unsupported_record",
                "invalid checkout recovery scope",
            )
            .into());
        }
        let parsed = |id| Res::Ok(Index::parse(&self.read_snapshot(id)?, self.store.algo()?)?);
        let (restored, current) = (parsed(&intent.index_before)?, parsed(&intent.index_after)?);
        if hash(&canonical_index(self.index()?, self.store.algo()?)) != intent.index_after {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "recovery precondition failed: index changed",
            )
            .into());
        }
        let paths: std::collections::HashSet<&[u8]> =
            intent.files.iter().map(|c| c.path.as_slice()).collect();
        for change in &intent.files {
            let path = BString::from(change.path.clone());
            self.ensure_path_allowed(&path)?;
            for state in [&change.before, &change.after].into_iter().flatten() {
                if state.mode != 0o160000 {
                    self.read_snapshot(&state.contents)?;
                }
            }
            if self.observed_among(&paths, change)? != change.after {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    format!("recovery precondition failed: working-tree path '{path}' changed"),
                )
                .into());
            }
        }
        let back = intent.files.iter().map(|change| FileChange {
            path: change.path.clone(),
            before: change.after.clone(),
            after: change.before.clone(),
        });
        let back = back.collect();
        let inverse = CheckoutIntent {
            version: 1,
            workspace: intent.workspace.clone(),
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: intent.index_after.clone(),
            index_after: intent.index_before.clone(),
            files: self.named_as_indexed(back, &restored, &current)?,
        };
        // a file it puts back has no directory that holds something in its place
        self.ensure_directories_replaceable(&inverse)?;
        Ok(inverse)
    }
}
