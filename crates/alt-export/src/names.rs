//! The names of the refs an export writes: git keeps each in a file of
//! its name, and so has no place for one beneath another.

use std::collections::HashSet;

use alt_refs::RefStore;

/// The first of the refs written that lies beneath another of them,
/// with that one.
pub(crate) fn beneath_another(refs: &RefStore) -> Option<(String, String)> {
    let written = |name: &&str| {
        *name != "HEAD" && crate::stash::entry_number(name).is_none() && !crate::stands_apart(name)
    };
    let mut names: Vec<&str> = refs.iter().map(|(name, _)| name).filter(written).collect();
    names.sort_unstable();
    let held: HashSet<&str> = names.iter().copied().collect();
    names.iter().find_map(|name| {
        let mut above = name.match_indices('/').map(|(at, _)| &name[..at]);
        let above = above.find(|above| held.contains(above))?;
        Some(((*name).to_owned(), above.to_owned()))
    })
}
