//! What a search reports of a match.

use std::ops::Range;

use crate::pike;

/// Where a match and its groups lie in the text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Captures {
    pub(crate) slots: Slots,
}

/// The slots of a few groups lie in the value itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Slots {
    Few([usize; FEW]),
    Many(Vec<usize>),
}

pub(crate) const FEW: usize = 8;

impl Captures {
    /// The span of group `index`; group 0 is the whole match. `None` for
    /// a group that took no part in the match.
    pub fn get(&self, index: usize) -> Option<Range<usize>> {
        let slots = match &self.slots {
            Slots::Few(slots) => slots.as_slice(),
            Slots::Many(slots) => slots.as_slice(),
        };
        let from = *slots.get(2 * index)?;
        let to = *slots.get(2 * index + 1)?;
        (from != pike::UNSET && to != pike::UNSET).then_some(from..to)
    }
}
