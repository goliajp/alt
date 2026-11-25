//! The objects some tips lead to, and how they are added to as the tips move.
//!
//! What a tip leads to is the same whenever it is asked: an object names what
//! it refers to by its content. So what the tips of one moment lead to is what
//! those of an earlier moment led to and what the tips that are new lead to
//! beside it, as long as every earlier tip is led to still. The walk from a tip
//! goes no further than an object that is led to already, and so takes as long
//! as what is new is large, whatever the repository holds.
//!
//! An object is put with those led to once everything it refers to is there.
//! Whenever the walk ends, with whatever reason, what it has come to is whole in
//! itself, and the next walk goes on from it.

use crate::{
    Error, Kind, Oid, OidSet, SegmentSet,
    object::{self, Parsed},
    trees::{Trees, entries},
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// The objects some tips lead to, among those the segments hold: with an
/// object, every object it refers to.
#[derive(Clone, Default)]
pub struct Led {
    /// each in the order of the ids, and no id in two of them; a part is less
    /// than half the one before it, so there are few
    parts: Vec<Arc<Vec<Oid>>>,
}

impl Led {
    pub fn to(&self, oid: &Oid) -> bool {
        (self.parts.iter()).any(|part| part.binary_search(oid).is_ok())
    }

    pub fn len(&self) -> usize {
        self.parts.iter().map(|part| part.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// every object, part by part
    pub fn objects(&self) -> impl Iterator<Item = &Oid> {
        self.parts.iter().flat_map(|part| part.iter())
    }

    /// The objects of a walk of tips that went before: `objects` as an earlier
    /// [`Self::objects`] gave them. None where an object is in it twice.
    pub fn again(mut objects: Vec<Oid>) -> Option<Self> {
        objects.sort_unstable();
        if objects.windows(2).any(|two| two[0] == two[1]) {
            return None;
        }
        let mut led = Self::default();
        led.add(objects);
        Some(led)
    }

    /// Adds `more`, none of which it has. The parts it shares with a copy of
    /// it are left as they are.
    fn add(&mut self, mut more: Vec<Oid>) {
        if more.is_empty() {
            return;
        }
        more.sort_unstable();
        while let Some(last) = self.parts.last()
            && last.len() < 2 * more.len()
        {
            let mut both = Vec::with_capacity(last.len() + more.len());
            let (mut ours, mut theirs) = (last.iter().peekable(), more.iter().peekable());
            while let (Some(one), Some(other)) = (ours.peek(), theirs.peek()) {
                both.push(
                    *if one < other { &mut ours } else { &mut theirs }
                        .next()
                        .unwrap(),
                );
            }
            both.extend(ours.chain(theirs));
            more = both;
            self.parts.pop();
        }
        self.parts.push(Arc::new(more));
    }
}

/// how many objects a walk comes to before it puts them with the rest
const AT_ONCE: usize = 1 << 20;
/// how many objects a walk comes to between two looks at whether it is waited for
const LOOKED_EVERY: usize = 4096;

struct Walk<'a> {
    led: Led,
    /// come to since the last were put with the rest
    late: OidSet,
    trees: Trees<'a>,
}

impl Walk<'_> {
    fn has(&self, oid: &Oid) -> bool {
        self.late.contains(oid) || self.led.to(oid)
    }

    fn put(&mut self, oid: Oid) {
        self.late.insert(oid);
        if self.late.len() >= AT_ONCE {
            self.led.add(self.late.drain().collect());
        }
    }
}

impl SegmentSet {
    /// Every object `tips` lead to. Reads `steps` trees at the most and ends once
    /// `gone` is set, as [`Self::select_while`] does.
    pub fn led_to(
        &self,
        tips: &[Oid],
        steps: usize,
        gone: Option<&AtomicBool>,
    ) -> Result<Led, Error> {
        let (led, ended) = self.led_beyond(Led::default(), tips, steps, gone);
        ended.map(|()| led)
    }

    /// What `tips` lead to, for one who has in `led` what other tips lead to
    /// and knows that `tips` lead to all of that too. Gives what it came to
    /// when it ended, and why it ended where that was before the last of it:
    /// what it gives then is led to by `tips` and whole in itself, and a walk
    /// that is given it goes on from there.
    pub fn led_beyond(
        &self,
        led: Led,
        tips: &[Oid],
        steps: usize,
        gone: Option<&AtomicBool>,
    ) -> (Led, Result<(), Error>) {
        let mut walk = Walk {
            led,
            late: OidSet::default(),
            trees: Trees::new(self, steps, gone),
        };
        let ended = self.come_to(&mut walk, tips, gone);
        let Walk { mut led, late, .. } = walk;
        led.add(late.into_iter().collect());
        (led, ended)
    }

    fn come_to(
        &self,
        walk: &mut Walk<'_>,
        tips: &[Oid],
        gone: Option<&AtomicBool>,
    ) -> Result<(), Error> {
        // an object, and whether what it refers to was gone to already: it is
        // come back to after all of that, and put with what is led to then
        let mut ahead: Vec<(Oid, bool)> = tips.iter().map(|tip| (*tip, false)).collect();
        let mut come = 0usize;
        while let Some((oid, back)) = ahead.pop() {
            if back {
                walk.put(oid);
                continue;
            }
            if walk.has(&oid) {
                continue;
            }
            come += 1;
            if come.is_multiple_of(LOOKED_EVERY) && gone.is_some_and(|gone| gone.load(Relaxed)) {
                return Err(Error::GivenUp);
            }
            match self.kind(&oid)?.ok_or(Error::NotFound(oid))? {
                Kind::Blob => walk.put(oid),
                Kind::Commit => {
                    let commit = self.commit(&oid).ok_or(Error::Corrupt("commit graph"))?;
                    ahead.push((oid, true));
                    // the parents before the tree: most of a tree is theirs
                    ahead.push((commit.tree, false));
                    ahead.extend(commit.parents.iter().map(|parent| (*parent, false)));
                }
                Kind::Tree => {
                    let data = walk.trees.read(&oid)?;
                    ahead.push((oid, true));
                    for entry in entries(&data)? {
                        if entry.tree {
                            ahead.push((entry.oid, false));
                        } else if !walk.has(&entry.oid) {
                            walk.put(entry.oid);
                        }
                    }
                }
                Kind::Tag => {
                    let (kind, data) = self.read(&oid)?;
                    let Parsed::Tag { object, .. } = object::parse(kind, &data)? else {
                        unreachable!()
                    };
                    ahead.push((oid, true));
                    ahead.push((object, false));
                }
            }
        }
        Ok(())
    }

    /// Whether `led` has every object the segments hold.
    pub fn all_led(&self, led: &Led) -> Result<bool, Error> {
        // an object may be in two segments, and is in one of them at the least
        let most = self.segments().iter().map(|segment| segment.len()).max();
        if led.len() < most.unwrap_or(0) {
            return Ok(false);
        }
        for segment in self.segments() {
            for index in 0..segment.len() {
                if !led.to(&segment.record(index)?.oid) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> Oid {
        let mut oid = [0; 20];
        oid[..4].copy_from_slice(&n.to_be_bytes());
        oid
    }

    #[test]
    fn what_is_added_is_found_with_what_was_there_and_the_parts_stay_few() {
        let mut led = Led::default();
        assert!(led.is_empty() && !led.to(&id(1)));
        assert_eq!(led.len(), 0);
        led.add(Vec::new());
        assert!(led.parts.is_empty());
        // a great part, and then a few at a time, as pushes bring them
        led.add((0..10_000).map(|n| id(n * 2)).collect());
        let before = led.clone();
        for n in 0..300 {
            led.add(vec![id(n * 2 + 1)]);
        }
        assert_eq!(led.len(), 10_300);
        assert!((0..10_000).all(|n| led.to(&id(n * 2))));
        assert!((0..300).all(|n| led.to(&id(n * 2 + 1))));
        assert!(!led.to(&id(601)) && !led.to(&id(20_000)));
        for part in &led.parts {
            assert!(part.is_sorted());
        }
        assert!(led.parts.len() <= 10, "{}", led.parts.len());
        let halves = led.parts.windows(2);
        assert!(halves.clone().all(|two| two[0].len() >= 2 * two[1].len()));
        // the copy taken before has what it had, and shares the great part
        assert_eq!(before.len(), 10_000);
        assert!(!before.to(&id(1)));
        assert!(Arc::ptr_eq(&before.parts[0], &led.parts[0]));
        // parts of a like size become one
        let mut even = Led::default();
        even.add(vec![id(5), id(1)]);
        even.add(vec![id(4), id(0)]);
        assert_eq!(even.parts.len(), 1);
        assert_eq!(*even.parts[0], [id(0), id(1), id(4), id(5)]);
    }
}
