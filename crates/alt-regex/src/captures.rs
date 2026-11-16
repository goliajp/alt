//! What a search reports of a match.

use std::ops::Range;

use crate::{Cache, Error, Regex, pike};

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

impl Regex {
    /// As [`Regex::captures_upto`], with the caller's storage.
    pub fn captures_upto_with(
        &self,
        cache: &mut Cache,
        text: &[u8],
        last: usize,
    ) -> Option<Captures> {
        let width = 2 * (last.min(self.groups()) + 1);
        if width <= FEW {
            let mut few = [pike::UNSET; FEW];
            let found = self.search(cache, text, &mut few[..width]) == Some(true);
            return found.then_some(Captures {
                slots: Slots::Few(few),
            });
        }
        let mut slots = vec![pike::UNSET; width];
        let found = self.search(cache, text, &mut slots) == Some(true);
        found.then_some(Captures {
            slots: Slots::Many(slots),
        })
    }

    /// As [`Regex::captures_upto_with`], and a search given up at its
    /// bounds is told apart from a text without a match: the other
    /// searches report the two alike.
    pub fn try_captures_upto_with(
        &self,
        cache: &mut Cache,
        text: &[u8],
        last: usize,
    ) -> Result<Option<Captures>, Error> {
        let width = 2 * (last.min(self.groups()) + 1);
        let (found, slots) = if width <= FEW {
            let mut few = [pike::UNSET; FEW];
            let found = self.search(cache, text, &mut few[..width]);
            (found, Slots::Few(few))
        } else {
            let mut many = vec![pike::UNSET; width];
            let found = self.search(cache, text, &mut many);
            (found, Slots::Many(many))
        };
        match found {
            Some(true) => Ok(Some(Captures { slots })),
            Some(false) => Ok(None),
            None => Err(Error::Exhausted),
        }
    }
}
