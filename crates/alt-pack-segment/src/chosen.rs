//! Which entry of a pack each of its objects is read from.
//!
//! A pack may carry an object in several entries: whole in one, a delta in another,
//! at the end of a long chain in a third. The segment keeps one record for the
//! object, and a delta made of the object is read by way of that record. Which
//! entry the record is of is chosen here, after every delta was resolved: the one
//! with the fewest deltas between it and a plain entry, the earliest of those. The
//! depth a record tells is then the deltas a read of it goes through, objects are
//! not made of one another in a circle, and what is chosen depends on the pack
//! alone, not on the order its deltas happened to be resolved in.

use crate::{Error, Oid, SegmentSet, ingest::Meta, scan::Form, scan::Raw};

pub(crate) struct Chosen {
    /// the entry of each object, the objects in the order of their ids
    pub kept: Vec<usize>,
    /// the deltas below each of them
    pub depth: Vec<u16>,
    /// by entry: the place of its object among `kept`
    pub position: Vec<u32>,
    /// whether an object has more than one entry
    pub duplicates: bool,
}

const UNSET: usize = usize::MAX;

/// The deltas below an entry that is made of an object of an earlier segment.
fn outside(raw: &Raw, existing: &SegmentSet, most: u16) -> Result<usize, Error> {
    let Form::Reference(base) = raw.form else {
        return Err(Error::Format("delta without a resolvable base"));
    };
    let below = existing.record(&base)?.ok_or(Error::MissingBase(base))?;
    if below.depth >= most {
        return Err(Error::Limit("delta depth"));
    }
    Ok(usize::from(below.depth) + 1)
}

/// `metas` and `entries` are of the same entries. An object more than `most`
/// deltas below a plain entry, by its shortest way, refuses the pack.
pub(crate) fn choose(
    metas: &[Meta],
    entries: &[Raw],
    existing: &SegmentSet,
    most: u16,
) -> Result<Chosen, Error> {
    let mut order: Vec<usize> = (0..metas.len()).collect();
    order.sort_unstable_by(|a, b| metas[*a].oid.cmp(&metas[*b].oid).then(a.cmp(b)));
    let mut position = vec![0u32; metas.len()];
    let mut objects = 0usize;
    let mut last: Option<&Oid> = None;
    for index in order {
        if last != Some(&metas[index].oid) {
            objects += 1;
            last = Some(&metas[index].oid);
        }
        position[index] = (objects - 1) as u32;
    }
    // the entries made of each object, object by object
    let mut starts = vec![0u32; objects + 1];
    for meta in metas {
        if let Some(base) = meta.base {
            starts[position[base] as usize + 1] += 1;
        }
    }
    for object in 0..objects {
        starts[object + 1] += starts[object];
    }
    let mut next = starts.clone();
    let mut made_of = vec![0u32; starts[objects] as usize];
    // entries by the deltas below them, as far as that is known without the others
    let mut levels: Vec<Vec<u32>> = vec![Vec::new()];
    for (index, meta) in metas.iter().enumerate() {
        let level = match (meta.base, meta.outside) {
            (Some(base), _) => {
                let slot = &mut next[position[base] as usize];
                made_of[*slot as usize] = index as u32;
                *slot += 1;
                continue;
            }
            (None, false) => 0,
            (None, true) => outside(&entries[index], existing, most)?,
        };
        if levels.len() <= level {
            levels.resize_with(level + 1, Vec::new);
        }
        levels[level].push(index as u32);
    }
    let mut kept = vec![UNSET; objects];
    let mut depth = vec![0u16; objects];
    let mut left = objects;
    let mut level = 0;
    while level < levels.len() {
        let here = std::mem::take(&mut levels[level]);
        let mut settled = Vec::new();
        for index in here {
            let object = position[index as usize] as usize;
            if kept[object] == UNSET {
                settled.push(object);
                depth[object] = level as u16;
                kept[object] = index as usize;
            } else if usize::from(depth[object]) == level {
                kept[object] = kept[object].min(index as usize);
            }
        }
        left -= settled.len();
        let below: Vec<u32> = (settled.iter())
            .flat_map(|object| &made_of[starts[*object] as usize..starts[*object + 1] as usize])
            .copied()
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
    Ok(Chosen {
        kept,
        depth,
        position,
        duplicates: objects < metas.len(),
    })
}
