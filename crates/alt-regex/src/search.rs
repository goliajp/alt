//! Which of the matchers answers a search. An answer of `None` is a
//! search given up at its bounds.

use crate::program::Program;
use crate::{Cache, FEW, Regex, backref, backtrack, dfa, pike};

impl Regex {
    #[inline]
    pub(crate) fn excluded(&self, text: &[u8]) -> bool {
        self.literal
            .as_ref()
            .is_some_and(|literal| literal.find(text).is_none())
    }

    /// What the bytes every match contains tell of `text` by
    /// themselves: no match where they are missing, and one where they
    /// are all the pattern is.
    #[inline]
    pub(crate) fn settled(&self, text: &[u8]) -> Option<bool> {
        let found = self.literal.as_ref()?.find(text).is_some();
        (!found || self.alone).then_some(found)
    }

    /// Whether there is a match, of a text [`Regex::settled`] left open.
    #[inline]
    pub(crate) fn decide(&self, cache: &mut Cache, text: &[u8]) -> Option<bool> {
        if text.is_empty() {
            return Some(self.on_empty.is_some());
        }
        if self.prog.backrefs {
            return backref::search(&self.prog, text, None, &mut [pike::UNSET; 2]);
        }
        if self.tables {
            return Some(dfa::earliest_end(&self.prog, &mut cache.scan, text).is_some());
        }
        self.sought(cache, text)
    }

    /// Whether there is a match, of a pattern the tables that step from
    /// every start at once do not answer for: few are such, and what
    /// they need is kept out of the steps of the others.
    #[cold]
    #[inline(never)]
    fn sought(&self, cache: &mut Cache, text: &[u8]) -> Option<bool> {
        if self.prog.plain_looks {
            return self.search(cache, text, &mut [pike::UNSET; 2]);
        }
        stepped(&self.prog, cache, text, None, &mut [pike::UNSET; 2])
    }

    /// The match of a pattern by what every match of it begins with,
    /// in a text where that first lies at `first` or later: a match
    /// begins where it lies, and the tables tell from behind it where
    /// the match ends. `None` where trying those places costs more
    /// than a search of the whole text.
    pub(crate) fn begun(
        &self,
        cache: &mut Cache,
        text: &[u8],
        first: usize,
    ) -> Option<Option<(usize, usize)>> {
        let (lead, behind) = self.lead.as_ref().expect("what it begins with");
        let mut work = 8 * text.len() + 64;
        for from in lead.lies(text, first) {
            if self.prog.apart && crate::chars::inside(text, from) {
                continue;
            }
            let place = (*behind, from + lead.len());
            let end = dfa::longest_behind(&self.prog, &mut cache.tail, text, place, &mut work);
            if let Some(end) = end {
                return Some(Some((from, end)));
            }
            if work == 0 {
                return None;
            }
        }
        Some(None)
    }

    /// Where the earliest match begins and where the longest one from
    /// there ends, as far as the tables could tell; `None` when there is
    /// no match. The text is one the tables can search.
    fn locate(&self, cache: &mut Cache, text: &[u8]) -> Option<(Option<usize>, Option<usize>)> {
        // trying each start costs what the text is long in the usual
        // case; a pattern for which it costs more is left to the slower,
        // steady search
        let mut work = 8 * text.len() + 64;
        if self.prog.anchored {
            let end = dfa::longest(&self.prog, &mut cache.fixed, text, 0, &mut work)?;
            return Some((Some(0), Some(end)));
        }
        // a match ends by here, so the earliest one begins by here
        let ends = dfa::earliest_end(&self.prog, &mut cache.scan, text)?;
        for from in 0..=ends {
            if let Some(end) = dfa::longest(&self.prog, &mut cache.fixed, text, from, &mut work) {
                return Some((Some(from), Some(end)));
            }
            if work == 0 {
                break;
            }
        }
        Some((None, None))
    }

    /// Fills `slots` with the capture slots of the earliest, longest
    /// match; false when there is none.
    #[inline]
    pub(crate) fn search(
        &self,
        cache: &mut Cache,
        text: &[u8],
        slots: &mut [usize],
    ) -> Option<bool> {
        if self.prog.backrefs {
            if self.excluded(text) {
                return Some(false);
            }
            return backref::search(&self.prog, text, None, slots);
        }
        if text.is_empty() {
            let Some(known) = &self.on_empty else {
                return Some(false);
            };
            if slots.len() <= FEW {
                slots.copy_from_slice(&known[..slots.len()]);
                return Some(true);
            }
            return grouped(&self.prog, cache, text, 0, 0, slots);
        }
        if self.alone {
            let fixed = self.literal.as_ref().expect("the bytes it is");
            let Some(at) = fixed.find(text) else {
                return Some(false);
            };
            slots.fill(pike::UNSET);
            slots[..2].copy_from_slice(&[at, at + fixed.len()]);
            return Some(true);
        }
        if let Some(fixed) = &self.literal {
            let Some(first) = fixed.find(text) else {
                return Some(false);
            };
            if self.led {
                match self.begun(cache, text, first) {
                    Some(None) => return Some(false),
                    Some(Some(span)) => return self.between(cache, text, span, slots),
                    None => {}
                }
            }
        }
        let mut from = None;
        if self.scanned {
            // most texts match nothing, which the tables tell fastest;
            // and where a match lies, which leaves only its groups
            let Some((begins, ends)) = self.locate(cache, text) else {
                return Some(false);
            };
            if let (Some(begins), Some(ends)) = (begins, ends) {
                if slots.len() == 2 {
                    slots.copy_from_slice(&[begins, ends]);
                    return Some(true);
                }
                return grouped(&self.prog, cache, text, begins, ends, slots);
            }
            from = begins;
        } else if self.long {
            return self.in_turn(cache, text, slots);
        }
        stepped(&self.prog, cache, text, from, slots)
    }

    /// Fills `slots` with the groups of the match that lies at `span`.
    fn between(
        &self,
        cache: &mut Cache,
        text: &[u8],
        (begins, ends): (usize, usize),
        slots: &mut [usize],
    ) -> Option<bool> {
        if slots.len() == 2 {
            slots.copy_from_slice(&[begins, ends]);
            return Some(true);
        }
        grouped(&self.prog, cache, text, begins, ends, slots)
    }

    /// The search of a program so long that stepping its paths from
    /// every start at once costs more than trying the starts in turn,
    /// as long as that costs no more than the text is long.
    #[cold]
    fn in_turn(&self, cache: &mut Cache, text: &[u8], slots: &mut [usize]) -> Option<bool> {
        if self.lead.is_some() {
            match self.begun(cache, text, 0) {
                Some(None) => return Some(false),
                Some(Some(span)) => return self.between(cache, text, span, slots),
                None => {}
            }
        }
        let mut work = 8 * text.len() + 64;
        for from in 0..=text.len() {
            let end = dfa::longest(&self.prog, &mut cache.fixed, text, from, &mut work);
            if let Some(end) = end {
                return self.between(cache, text, (from, end), slots);
            }
            if work == 0 {
                break;
            }
        }
        if work > 0 {
            return Some(false);
        }
        match self.locate(cache, text) {
            None => Some(false),
            Some((Some(begins), Some(ends))) => self.between(cache, text, (begins, ends), slots),
            Some((from, _)) => stepped(&self.prog, cache, text, from, slots),
        }
    }
}

/// Steps every path at once, from `from` or from wherever a match
/// begins.
fn stepped(
    prog: &Program,
    cache: &mut Cache,
    text: &[u8],
    mut from: Option<usize>,
    slots: &mut [usize],
) -> Option<bool> {
    let carried = slots.len() * (1 + usize::from(prog.snapshots));
    // the paths of every start together would carry too much: where the
    // match begins is found first, with no groups, and then the groups
    // of the paths from there
    if from.is_none() && slots.len() > 2 && prog.row_count as usize * carried > pike::MAX_LIST {
        let mut whole = [pike::UNSET; 2];
        if !stepped(prog, cache, text, None, &mut whole)? {
            return Some(false);
        }
        from = Some(whole[0]);
    }
    let found = pike::search(prog, &mut cache.pike, text, from, slots);
    (!cache.pike.exhausted).then_some(found)
}

/// Fills `slots` with the groups of the match from `from` to `to`.
fn grouped(
    prog: &Program,
    cache: &mut Cache,
    text: &[u8],
    from: usize,
    to: usize,
    slots: &mut [usize],
) -> Option<bool> {
    if backtrack::fits(prog, to - from) {
        let tried = backtrack::captures(prog, &mut cache.backtrack, text, from, to, slots);
        if tried.is_some() {
            return tried;
        }
        slots.fill(pike::UNSET);
    }
    stepped(prog, cache, text, Some(from), slots)
}

/// The first capture slots of the match in the empty text, if it has
/// one: a text with nothing in it is common, and always answered alike.
pub(crate) fn on_empty(prog: &Program) -> Option<[usize; FEW]> {
    let mut slots = [pike::UNSET; FEW];
    let width = FEW.min(2 * (prog.groups as usize + 1));
    let matched = match prog.backrefs {
        // whether it matches is all that is asked of such a pattern here
        true => backref::search(prog, b"", None, &mut slots[..2]),
        false => grouped(prog, &mut Cache::default(), b"", 0, 0, &mut slots[..width]),
    };
    (matched == Some(true)).then_some(slots)
}
