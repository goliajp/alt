//! The objects a pack refers to, until each is found among those it carries or
//! those the segments already present hold.
//!
//! What a pack refers to is many times what it carries: every entry of every tree,
//! and a pack made for it inflates to trees without end. Whether an id is carried
//! is known only when the last delta is resolved, so the ids wait, each once. They
//! wait within a bound: an id no segment present holds has to be an object of the
//! pack, and there are no more of those than the pack has entries.
//!
//! Segments keep what no ref names any more. Where the pack comes from one who
//! is led to less than the segments hold, what they hold beyond that is not
//! there for the pack: an id is no proof of having the object, and a ref that
//! came to name it would hand it out.

use crate::{Error, Kind, Led, Oid, OidMap, SegmentSet, carried::Carried};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering::Relaxed},
};

const SHARDS: usize = 64;
/// ids of one shard looked up in the segments present together
const TOGETHER: usize = 4096;

#[derive(Default)]
struct Shard {
    /// not looked up yet: the kind it is referred to as, and the entry that does
    fresh: OidMap<(Kind, u32)>,
    /// no segment present holds it as that kind: the pack has to
    own: OidMap<(Kind, u32)>,
}

pub(crate) struct Wanted<'a> {
    existing: &'a SegmentSet,
    /// what of `existing` the sender of the pack is led to, where not all of it
    led: Option<&'a Led>,
    /// the entries of the pack
    entries: usize,
    shards: Vec<Mutex<Shard>>,
    /// the ids the pack has to carry, in every shard
    own: AtomicUsize,
}

impl<'a> Wanted<'a> {
    pub fn new(existing: &'a SegmentSet, led: Option<&'a Led>, entries: usize) -> Self {
        Wanted {
            existing,
            led,
            entries,
            shards: (0..SHARDS).map(|_| Mutex::default()).collect(),
            own: AtomicUsize::new(0),
        }
    }

    /// the kind of an object the segments present hold for the sender of the pack
    fn held(&self, oid: &Oid) -> Result<Option<Kind>, Error> {
        match self.led {
            Some(led) if !led.to(oid) => Ok(None),
            _ => self.existing.kind(oid),
        }
    }

    /// The entry `from`, the object `of`, refers to `to` as a `kind`.
    pub fn want(&self, from: u32, of: &Oid, to: Oid, kind: Kind) -> Result<(), Error> {
        // the peer chooses the ids; a shard that gets them all is only slower
        let mut shard = self.shards[usize::from(to[1]) % SHARDS]
            .lock()
            .expect("wanted");
        let shard = &mut *shard;
        if let Some((known, _)) = shard.own.get(&to).or_else(|| shard.fresh.get(&to)) {
            // one id is one kind of object
            return if *known == kind {
                Ok(())
            } else {
                Err(Error::Dangling(*of, to))
            };
        }
        shard.fresh.insert(to, (kind, from));
        if shard.fresh.len() < TOGETHER {
            return Ok(());
        }
        let mut own = 0;
        for (to, (kind, from)) in shard.fresh.drain() {
            if self.held(&to)? != Some(kind) {
                shard.own.insert(to, (kind, from));
                own += 1;
            }
        }
        if self.own.fetch_add(own, Relaxed) + own > self.entries {
            return Err(Error::Dangling(*of, to));
        }
        Ok(())
    }

    /// Fails unless every object referred to is carried or held by a segment
    /// present, as the kind it is referred to as. `of` is the id of an entry.
    /// The objects held and not carried, each once: what the pack leans on.
    pub fn check(
        self,
        carried: &Carried,
        of: impl Fn(u32) -> Oid + Sync,
    ) -> Result<Vec<(Oid, Kind)>, Error> {
        let leans = Mutex::new(Vec::new());
        alt_threads::try_for_each(&self.shards, 1, |shard| {
            let shard = shard.lock().expect("wanted");
            for (to, (kind, from)) in &shard.own {
                if carried.kind(to) != Some(*kind) {
                    return Err(Error::Dangling(of(*from), *to));
                }
            }
            let mut held = Vec::new();
            for (to, (kind, from)) in &shard.fresh {
                let found = match carried.kind(to) {
                    Some(found) => Some(found),
                    None => {
                        held.push((*to, *kind));
                        self.held(to)?
                    }
                };
                if found != Some(*kind) {
                    return Err(Error::Dangling(of(*from), *to));
                }
            }
            leans.lock().expect("leans").extend(held);
            Ok(())
        })?;
        Ok(leans.into_inner().expect("leans"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> Oid {
        let mut id = [7u8; 20];
        id[4..8].copy_from_slice(&n.to_be_bytes());
        id
    }

    #[test]
    fn the_ids_that_wait_are_each_there_once_and_no_more_than_a_pack_can_carry() {
        let none = SegmentSet::default();
        let from = id(u32::MAX);
        // as many as a pack of this many entries can carry, each named many times
        let wanted = Wanted::new(&none, None, 3 * TOGETHER);
        for _ in 0..4 {
            for n in 0..3 * TOGETHER as u32 {
                wanted.want(9, &from, id(n), Kind::Blob).unwrap();
            }
        }
        assert_eq!(wanted.own.load(Relaxed), 3 * TOGETHER);
        let held: usize = (wanted.shards.iter())
            .map(|shard| {
                let shard = shard.lock().unwrap();
                shard.fresh.len() + shard.own.len()
            })
            .sum();
        assert_eq!(held, 3 * TOGETHER);
        // one id is one kind of object
        let other = wanted.want(9, &from, id(0), Kind::Tree);
        assert!(matches!(other, Err(Error::Dangling(a, b)) if a == from && b == id(0)));
        // carried, all of them; and one of them not
        let all = Carried::new((0..3 * TOGETHER as u32).map(|n| (id(n), Kind::Blob)));
        let but_one = Carried::new((1..3 * TOGETHER as u32).map(|n| (id(n), Kind::Blob)));
        let wanted = |carried: &Carried| {
            let wanted = Wanted::new(&none, None, 3 * TOGETHER);
            for n in 0..3 * TOGETHER as u32 {
                wanted.want(n, &from, id(n), Kind::Blob).unwrap();
            }
            wanted.check(carried, id)
        };
        assert!(wanted(&all).unwrap().is_empty());
        let missing = wanted(&but_one);
        assert!(matches!(missing, Err(Error::Dangling(a, b)) if a == id(0) && b == id(0)));

        // more ids that no segment has than the pack has entries: refused when the
        // ids are looked up, and not when the last object was resolved
        let wanted = Wanted::new(&none, None, TOGETHER - 1);
        let refused =
            (0..TOGETHER as u32).try_for_each(|n| wanted.want(9, &from, id(n), Kind::Blob));
        assert!(matches!(refused, Err(Error::Dangling(a, _)) if a == from));
    }
}
