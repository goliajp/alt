//! Which entry of a pack each of its objects is read from.
//!
//! A pack may carry an object in several entries: whole in one, a delta in another,
//! at the end of a long chain in a third. The segment keeps one record for the
//! object, and a delta made of the object is read by way of that record. Which
//! entry the record is of is chosen here, after every delta was resolved: the one
//! with the fewest deltas between it and a plain entry, the earliest of those. The
//! depth a record tells is then the deltas a read of it goes through, objects are
//! not made of one another in a circle, and what is chosen depends on the pack
//! and the segments before it alone, not on the order its deltas happened to be
//! resolved in.
//!
//! A delta that names its base by id may have two ways down: the pack may carry
//! that object, and an earlier segment may hold it. Which of them resolved the
//! delta is chance; which the record goes by is not: the one with fewer deltas
//! below it, the earlier segment where they are as many.

use crate::{Error, Oid, format::NO_BASE, ingest::Meta, scan::Form};

pub(crate) struct Chosen {
    /// the entry of each object, the objects in the order of their ids
    pub kept: Vec<usize>,
    /// the deltas below each of them
    pub depth: Vec<u16>,
    /// what each of them is made of: the place among `kept` of an object of the
    /// pack, or `NO_BASE` for a plain entry and for a delta of an earlier segment
    pub base: Vec<u32>,
    /// whether each of them is a delta of an object of an earlier segment
    pub outside: Vec<bool>,
    /// whether an object has more than one entry
    pub duplicates: bool,
}

const UNSET: usize = usize::MAX;

/// A way an entry is come to, with the deltas there are below it that way.
#[derive(Clone, Copy)]
struct Way {
    entry: u32,
    /// by an earlier segment, not by an object of the pack
    outside: bool,
}

/// `metas` and `form` are of the same entries; `earlier` are the objects of
/// earlier segments that deltas of the pack name, in the order of their ids, each
/// with the deltas below it there. An object more than `most` deltas below a
/// plain entry, by its shortest way, refuses the pack.
pub(crate) fn choose(
    metas: &[Meta],
    form: impl Fn(usize) -> Form,
    earlier: &[(Oid, u16)],
    most: u16,
) -> Result<Chosen, Error> {
    let mut order: Vec<usize> = (0..metas.len()).collect();
    order.sort_unstable_by(|a, b| metas[*a].oid.cmp(&metas[*b].oid).then(a.cmp(b)));
    let mut position = vec![0u32; metas.len()];
    let mut ids: Vec<&Oid> = Vec::new();
    for index in order {
        if ids.last() != Some(&&metas[index].oid) {
            ids.push(&metas[index].oid);
        }
        position[index] = (ids.len() - 1) as u32;
    }
    let objects = ids.len();
    // what each entry is made of in the pack, and how deep in an earlier segment
    let within = |index: usize| match (metas[index].base, form(index)) {
        (Some(base), _) => Some(position[base]),
        (None, Form::Reference(base)) => ids.binary_search(&&base).ok().map(|at| at as u32),
        (None, _) => None,
    };
    let without = |index: usize| match form(index) {
        Form::Reference(base) => {
            (earlier.binary_search_by(|held| held.0.cmp(&base)).ok()).map(|at| earlier[at].1)
        }
        _ => None,
    };
    // the entries made of each object, object by object
    let mut starts = vec![0u32; objects + 1];
    for index in 0..metas.len() {
        if let Some(base) = within(index) {
            starts[base as usize + 1] += 1;
        }
    }
    for object in 0..objects {
        starts[object + 1] += starts[object];
    }
    let mut next = starts.clone();
    let mut made_of = vec![0u32; starts[objects] as usize];
    // entries by the deltas below them, as far as that is known without the others
    let mut levels: Vec<Vec<Way>> = vec![Vec::new()];
    let mut put = |level: usize, way: Way| {
        if levels.len() <= level {
            levels.resize_with(level + 1, Vec::new);
        }
        levels[level].push(way);
    };
    for index in 0..metas.len() {
        let entry = index as u32;
        let inside = within(index);
        if let Some(base) = inside {
            let slot = &mut next[base as usize];
            made_of[*slot as usize] = entry;
            *slot += 1;
        }
        match (form(index), without(index)) {
            (Form::Plain(_), _) => put(
                0,
                Way {
                    entry,
                    outside: false,
                },
            ),
            (_, Some(below)) if below < most => {
                put(
                    usize::from(below) + 1,
                    Way {
                        entry,
                        outside: true,
                    },
                );
            }
            (_, Some(_)) if inside.is_none() => return Err(Error::Limit("delta depth")),
            (Form::Reference(base), None) if inside.is_none() => {
                return Err(Error::MissingBase(base));
            }
            (_, None) if inside.is_none() => {
                return Err(Error::Format("delta without a resolvable base"));
            }
            _ => {}
        }
    }
    let mut kept = vec![UNSET; objects];
    let mut depth = vec![0u16; objects];
    let mut outside = vec![false; objects];
    let mut left = objects;
    let mut level = 0;
    while level < levels.len() {
        let here = std::mem::take(&mut levels[level]);
        let mut settled = Vec::new();
        for way in here {
            let (index, object) = (way.entry as usize, position[way.entry as usize] as usize);
            if kept[object] == UNSET {
                settled.push(object);
                depth[object] = level as u16;
            } else if usize::from(depth[object]) != level {
                continue;
            }
            // the earliest entry, and of its two ways the earlier segment
            if index < kept[object] || (index == kept[object] && way.outside) {
                kept[object] = index;
                outside[object] = way.outside;
            }
        }
        left -= settled.len();
        // an entry of an object that is settled is not looked at again
        let below: Vec<Way> = (settled.iter())
            .flat_map(|object| &made_of[starts[*object] as usize..starts[*object + 1] as usize])
            .filter(|entry| kept[position[**entry as usize] as usize] == UNSET)
            .map(|entry| Way {
                entry: *entry,
                outside: false,
            })
            .collect();
        if !below.is_empty() {
            if level >= usize::from(most) {
                return Err(Error::Limit("delta depth"));
            }
            if levels.len() == level + 1 {
                levels.push(Vec::new());
            }
            levels[level + 1].extend(below);
        }
        level += 1;
    }
    if left > 0 {
        return Err(Error::Format("delta without a resolvable base"));
    }
    let base = (kept.iter().zip(&outside))
        .map(|(index, outside)| match (within(*index), outside) {
            (Some(base), false) => base,
            _ => NO_BASE,
        })
        .collect();
    Ok(Chosen {
        kept,
        depth,
        base,
        outside,
        duplicates: objects < metas.len(),
    })
}
