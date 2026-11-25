//! Finding frames of the account of a handle cut off the log (`owed`):
//! by a look at each frame owed, by a cut the handle makes itself, and
//! what its caller was told when the handle read the log anew.

use crate::v2::owed::{Account, Holder, Owed};

impl Account {
    pub(super) fn found_cut(&mut self, holder: Holder, unpromised: bool) {
        if !self.cut.contains(&(holder, unpromised)) {
            self.cut.push((holder, unpromised));
        }
    }

    /// The handle cut the log at byte `at` itself: of a frame it holds
    /// itself its caller is told by that cut.
    pub fn cut_at(&mut self, at: u64) {
        let owed = self.owed.iter().copied();
        let (kept, gone): (Vec<Owed>, Vec<Owed>) = owed.partition(|owed| owed.end <= at);
        self.owed = kept;
        let others = |owed: &&Owed| owed.holder != Holder::Own && !owed.told;
        for owed in gone.iter().filter(others) {
            self.found_cut(owed.holder, owed.unpromised);
        }
    }

    /// Whoever asks the handle with no ticket was told that frames were
    /// cut: the ones found cut, and the ones the last look could not
    /// tell of, which are owed a sync still where they are in the log.
    pub fn told(&mut self) {
        self.cut.retain(|(holder, _)| *holder != Holder::Own);
        for owed in &mut self.owed {
            owed.told |= owed.unsure && owed.holder == Holder::Own;
        }
    }

    /// Looks at every frame owed with `placed`, which says whether the
    /// frame is where it was written, or `None` where it cannot tell.
    /// One that is not was cut.
    pub fn look<E>(
        &mut self,
        mut placed: impl FnMut(&Owed) -> Result<Option<bool>, E>,
    ) -> Result<(), E> {
        let mut looked = Ok(());
        self.unsure = false;
        for mut owed in std::mem::take(&mut self.owed) {
            // what is not looked at stays owed
            let placed = match looked {
                Ok(()) => placed(&owed),
                Err(_) => Ok(None),
            };
            match placed {
                Ok(Some(false)) if owed.told => {}
                Ok(Some(false)) => self.found_cut(owed.holder, owed.unpromised),
                Ok(sure) => {
                    owed.unsure = sure.is_none();
                    self.unsure |= owed.unsure;
                    self.owed.push(owed);
                }
                Err(error) => {
                    self.owed.push(owed);
                    looked = Err(error);
                }
            }
        }
        looked
    }
}
