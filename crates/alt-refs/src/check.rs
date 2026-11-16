//! The full check of the ref store.

use std::collections::BTreeMap;

use crate::state::Refs;
use crate::{RefError, RefStore, fold_new};

/// What a full check read and verified.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RefsChecked {
    /// operations in the log
    pub ops: u64,
    pub refs: u64,
}

impl RefStore {
    /// The refs as they were before the last `dropped` ops of the log,
    /// replayed from the first op.
    pub fn refs_before(
        &self,
        dropped: usize,
    ) -> Result<BTreeMap<String, crate::RefTarget>, RefError> {
        let ops = self.oplog.ops()?;
        let mut replayed = Refs::default();
        let ops = &ops[..ops.len().saturating_sub(dropped)];
        fold_new(&mut replayed, &mut 0, &mut BTreeMap::new(), ops)?;
        Ok(replayed.to_map())
    }

    /// Reads and verifies every record of the operation log, then replays
    /// every ref transaction from the first one and compares the outcome
    /// with the refs this store serves. An ordinary open starts from the
    /// snapshot and reads only the log after it; this is what finds a
    /// damaged record before that point, or a snapshot that says something
    /// the log does not.
    pub fn check(&self) -> Result<RefsChecked, RefError> {
        let ops = self.oplog.ops()?;
        // ops another writer appended that are not folded into `refs` yet
        let unapplied = self.oplog.recent().len() - self.applied;
        let ops = &ops[..ops.len() - unapplied];
        let mut replayed = Refs::default();
        fold_new(&mut replayed, &mut 0, &mut BTreeMap::new(), ops)?;
        if !replayed.iter().eq(self.refs.iter()) {
            return Err(RefError::Format("refs disagree with the operation log"));
        }
        Ok(RefsChecked {
            ops: ops.len() as u64,
            refs: replayed.len() as u64,
        })
    }
}
