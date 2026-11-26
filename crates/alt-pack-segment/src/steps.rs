//! The steps a walk of the trees and the commits of a fetch takes.

use std::sync::atomic::Ordering::Relaxed;

use crate::Error;
use crate::trees::Trees;

impl Trees<'_> {
    /// whether nobody waits for the walk any more
    pub(crate) fn gone(&self) -> bool {
        self.gone.is_some_and(|gone| gone.load(Relaxed))
    }

    /// One step more of the walk: a tree read, or a commit gone through.
    pub(crate) fn step(&mut self) -> Result<(), Error> {
        let left = self.left.checked_sub(1);
        self.left = left.ok_or(Error::Limit("commits and trees of a fetch"))?;
        match self.gone() {
            true => Err(Error::GivenUp),
            false => Ok(()),
        }
    }
}
