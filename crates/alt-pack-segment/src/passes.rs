//! The passes that resolve the deltas of a pack, and how deep they go.
//!
//! How deep an object lies is known when every entry that makes it is: an
//! entry at the end of a long chain may make an object another entry makes
//! of a plain one. A pack whose chains go on beyond what deltas may lie below
//! an object is not resolved to its end to find that out. The walks stop one
//! delta beyond the limit and leave what is deeper. What was left is come to
//! again below each object that lies within the limit by a shorter way than
//! the walk came by, until nothing left is made of such an object. What is
//! left then is made of objects beyond the limit, whatever is resolved after:
//! the pack is refused. So the deltas resolved are those of the objects within
//! the limit and no others, whichever walk came to them first.

use crate::{
    Error, Kind, Oid, SegmentSet,
    chosen::choose,
    descend::{self, Root, Shared},
    format::{CommitRow, NO_BASE},
    ingest::Meta,
    scan::{Form, Raw},
    told::Told,
};
use alt_git_pack::{
    IndexWork,
    walk::{Budget, Deltas},
};
use std::sync::atomic::AtomicBool;

pub(crate) struct Passes<'a, 'b> {
    pub pack: &'a [u8],
    pub entries: &'a [Raw],
    pub existing: &'a SegmentSet,
    pub told: &'a Told<'a>,
    pub deltas: &'a Deltas<Oid>,
    pub budget: &'a Budget<'b>,
    /// the bases that earlier segments hold, in the order of their ids, and
    /// the deltas below each there
    pub outside: &'a [Oid],
    pub below: &'a [u16],
    pub walks: Option<usize>,
    /// the most deltas there may be below an object
    pub depth: u16,
}

impl Passes<'_, '_> {
    /// Resolves what is below the plain entries `roots` and the bases of
    /// earlier segments `outer`; `metas` has what the plain entries are.
    pub(crate) fn resolve(
        self,
        (roots, outer): (Vec<Root>, Vec<Root>),
        metas: &mut [Option<Meta>],
        commits: &mut Vec<CommitRow>,
    ) -> Result<IndexWork, Error> {
        let taken: Vec<AtomicBool> = (self.entries.iter())
            .map(|_| AtomicBool::new(false))
            .collect();
        let mut work = IndexWork::default();
        type Left = Vec<(u32, Kind)>;
        let mut pass = |roots: Vec<Root>, metas: &mut [Option<Meta>], left: &mut Left| {
            let shared = Shared {
                pack: self.pack,
                entries: self.entries,
                existing: self.existing,
                told: self.told,
                deltas: self.deltas,
                budget: self.budget,
                outside: self.outside,
                taken: &taken,
            };
            let made = descend::below(&shared, roots, self.walks)?;
            commits.extend(made.commits);
            work.inflated += made.work.inflated;
            work.applied += made.work.applied;
            work.hashed += made.work.hashed;
            for (index, meta) in made.metas {
                metas[index].get_or_insert(meta);
            }
            left.extend(made.left);
            Ok::<_, Error>(())
        };
        let mut left = Vec::new();
        pass(roots, metas, &mut left)?;
        pass(outer, metas, &mut left)?;
        while !left.is_empty() {
            left.sort_unstable_by_key(|(of, _)| *of);
            left.dedup_by_key(|(of, _)| *of);
            let again = self.within(metas, &mut left)?;
            pass(again, metas, &mut left)?;
        }
        (work.held, work.spilled) = self.budget.most();
        work.spilled_files = self.budget.most_files();
        Ok(work)
    }

    /// Of what the deltas that were left are made of, the objects that lie
    /// within the limit, as the roots of one more pass: `left` keeps the
    /// others. The refusal of the pack where there is none.
    fn within(
        &self,
        metas: &[Option<Meta>],
        left: &mut Vec<(u32, Kind)>,
    ) -> Result<Vec<Root>, Error> {
        let resolved: Vec<usize> = (0..metas.len())
            .filter(|index| metas[*index].is_some())
            .collect();
        let mut place = vec![usize::MAX; metas.len()];
        for (at, index) in resolved.iter().enumerate() {
            place[*index] = at;
        }
        let part: Vec<Meta> = (resolved.iter())
            .map(|index| {
                let meta = metas[*index].as_ref().expect("resolved");
                Meta {
                    base: meta.base.map(|base| place[base]),
                    ..Meta::plain(meta.oid, meta.blake3, meta.kind, meta.size)
                }
            })
            .collect();
        let earlier: Vec<(Oid, u16)> = (self.outside.iter().copied())
            .zip(self.below.iter().copied())
            .collect();
        let form = |at: usize| self.entries[resolved[at]].form;
        let chosen = choose(&part, form, &earlier, self.depth.saturating_add(1))?;
        let deep = |oid: &Oid| {
            let found = chosen.kept.binary_search_by(|at| part[*at].oid.cmp(oid));
            found.map_or(u16::MAX, |object| chosen.depth[object])
        };
        let base = |of: u32| match metas.get(of as usize) {
            Some(meta) => {
                let oid = meta.as_ref().expect("the base of a delta come to").oid;
                (oid, deep(&oid))
            }
            None => {
                let at = of as usize - metas.len();
                (self.outside[at], self.below[at])
            }
        };
        // each made again by the shortest way there is to it: the entries the
        // objects on that way are read from, from one that is no delta
        let way = |of: u32, oid: &Oid| {
            let Ok(mut object) = chosen.kept.binary_search_by(|at| part[*at].oid.cmp(oid)) else {
                return vec![of];
            };
            let mut way = Vec::new();
            loop {
                let entry = resolved[chosen.kept[object]];
                way.push(entry as u32);
                match (chosen.base[object], self.entries[entry].form) {
                    (NO_BASE, Form::Reference(base)) if chosen.outside[object] => {
                        let outside = self.outside.binary_search(&base);
                        way.push((metas.len() + outside.expect("the base it is made of")) as u32);
                        break;
                    }
                    (NO_BASE, _) => break,
                    (base, _) => object = base as usize,
                }
            }
            way.reverse();
            way
        };
        let mut again = Vec::new();
        left.retain(|(of, kind)| {
            let (oid, depth) = base(*of);
            if depth <= self.depth {
                again.push(Root {
                    entry: *of,
                    oid,
                    kind: *kind,
                    depth,
                    way: way(*of, &oid),
                });
            }
            depth > self.depth
        });
        match again.is_empty() {
            true => Err(Error::Limit("delta depth")),
            false => Ok(again),
        }
    }
}
