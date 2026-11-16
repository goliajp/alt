//! Which of the matchers answers a search.

use alt_bytes::ByteSlice;

use crate::{Cache, FEW, Regex, backref, backtrack, dfa, pike};

impl Regex {
    #[inline]
    pub(crate) fn excluded(&self, text: &[u8]) -> bool {
        self.literal
            .as_ref()
            .is_some_and(|literal| text.find(literal).is_none())
    }

    #[inline]
    pub(crate) fn decide(&self, cache: &mut Cache, text: &[u8]) -> bool {
        if text.is_empty() {
            return self.on_empty.is_some();
        }
        if self.prog.backrefs {
            return backref::search(&self.prog, text, None, &mut [pike::UNSET; 2]);
        }
        if self.prog.plain_looks {
            return dfa::earliest_end(&self.prog, &mut cache.scan, text).is_some();
        }
        pike::search(
            &self.prog,
            &mut cache.pike,
            text,
            None,
            &mut [pike::UNSET; 2],
        )
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
    pub(crate) fn search(&self, cache: &mut Cache, text: &[u8], slots: &mut [usize]) -> bool {
        if self.prog.backrefs {
            return !self.excluded(text) && backref::search(&self.prog, text, None, slots);
        }
        if text.is_empty() {
            let Some(known) = &self.on_empty else {
                return false;
            };
            if slots.len() <= FEW {
                slots.copy_from_slice(&known[..slots.len()]);
                return true;
            }
            return backtrack::captures(&self.prog, &mut cache.backtrack, text, 0, 0, slots);
        }
        if self.excluded(text) {
            return false;
        }
        let mut from = None;
        if self.prog.plain_looks {
            // most texts match nothing, which the tables tell fastest;
            // and where a match lies, which leaves only its groups
            let Some((begins, ends)) = self.locate(cache, text) else {
                return false;
            };
            if let (Some(begins), Some(ends)) = (begins, ends) {
                if slots.len() == 2 {
                    slots.copy_from_slice(&[begins, ends]);
                    return true;
                }
                if backtrack::fits(&self.prog, ends - begins) {
                    let prog = &self.prog;
                    return backtrack::captures(
                        prog,
                        &mut cache.backtrack,
                        text,
                        begins,
                        ends,
                        slots,
                    );
                }
            }
            from = begins;
        }
        pike::search(&self.prog, &mut cache.pike, text, from, slots)
    }
}
