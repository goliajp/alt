//! What a walk holds of the bases on its way, and what it lets go of.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use super::held::Base;
use super::objects::Objects;
use super::walk::Walk;
use crate::PackError;

impl<O: Objects> Walk<'_, O> {
    pub(super) fn hold(&mut self, bytes: usize) {
        let held = self.budget.held.fetch_add(bytes, Relaxed) + bytes;
        self.budget.most_held.fetch_max(held, Relaxed);
    }

    /// Takes what a frame gave up out of the counts.
    pub(super) fn gone(&mut self, data: Option<Arc<Base>>) {
        if let Some(data) = data {
            self.budget.held.fetch_sub(data.held(), Relaxed);
            self.budget.spilled.fetch_sub(data.spilled(), Relaxed);
            if data.spilled() > 0 {
                self.budget.spilled_files.fetch_sub(1, Relaxed);
            }
        }
    }

    /// Makes room while the bases held pass their limit, in those nearest
    /// the plain entry first: they are the last to be needed again. One
    /// that deltas still wait for goes to a file while there is room in
    /// those, for this pack and for all the packs there are, and is
    /// otherwise let go of; so is one whose file could not be written.
    /// The top frame keeps its own.
    ///
    /// Room among the files is waited for by a pack that has none there, in
    /// a walk that has no memory of an object taken: whoever waits has
    /// nothing another waits for.
    pub(super) fn let_go(&mut self) -> Result<(), PackError> {
        let top = self.stack.len() - 1;
        let budget = self.budget;
        while budget.held.load(Relaxed) > budget.base_bytes && self.lowest < top {
            let frame = &mut self.stack[self.lowest];
            let waited_for = !frame.ofs.is_empty() || !frame.refs.is_empty();
            if let Some(data) = frame.data.take_if(|data| data.held() > 0) {
                budget.held.fetch_sub(data.held(), Relaxed);
                let bytes = data.len() as u64;
                let room = |spilled: u64| {
                    (spilled.checked_add(bytes)).filter(|total| *total <= budget.spill_bytes)
                };
                let file = |files: usize| (files < budget.spill_files).then_some(files + 1);
                let files = &budget.spilled_files;
                if waited_for
                    && let Ok(files_before) = files.try_update(Relaxed, Relaxed, file)
                    && let Ok(before) = (budget.spilled.try_update(Relaxed, Relaxed, room))
                        .inspect_err(|_| _ = files.fetch_sub(1, Relaxed))
                {
                    let wait = (before == 0 && self.object.is_none()).then_some(budget.wait);
                    let written = match budget.files {
                        None => data.spill(None, None).ok(),
                        Some(files) => files.take(bytes, wait).and_then(|room| {
                            let written = data.spill(Some(files.directory()), Some(room));
                            if written.is_err() {
                                files.failed();
                            }
                            written.ok()
                        }),
                    };
                    match written {
                        Some(written) => {
                            budget.most_spilled.fetch_max(before + bytes, Relaxed);
                            (budget.most_spilled_files).fetch_max(files_before + 1, Relaxed);
                            frame.data = Some(Arc::new(written));
                        }
                        None => {
                            budget.spilled.fetch_sub(bytes, Relaxed);
                            files.fetch_sub(1, Relaxed);
                        }
                    }
                }
            }
            self.lowest += 1;
        }
        Ok(())
    }
}
