//! Which of what `git fsck` refuses a push brings anew.

use crate::{Entry, Fault, File, Flagged, Special, TREE};
use alt_git_codec::unsafe_name;
use std::collections::{BTreeMap, BTreeSet};

/// a commit a push brings: its tree, and the trees of its parents
pub struct Commit<O> {
    pub tree: O,
    pub parents: Vec<O>,
}

/// what gives the entries of a tree
pub type Trees<'a, O, E> = dyn FnMut(&O) -> Result<Vec<Entry<Vec<u8>, O>>, E> + 'a;
/// what tells the fault of the object a tree names as a special file
pub type Files<'a, O, E> = dyn FnMut(&O, Special) -> Result<Option<&'static str>, E> + 'a;

/// what of a flagged tree is refused unless the tree before it had it
struct Pending<'a, O> {
    faults: Vec<&'a Fault<O>>,
    files: Vec<(&'a File<O>, &'static str)>,
}

/// The first fault a push brings anew, with the object it is told of: of the
/// trees `flagged`, each as [`crate::flagged`] found it, and the files they
/// name, which `file` tells the fault of.
///
/// A fault of a tree, or of a file it names, is no fault of the push where a
/// tree at the same place in a parent of a commit of `commits` had it (see
/// [`Fault::had`], [`File::had`]). The trees of each commit are walked where
/// they differ from those of its parents; a flagged tree no commit comes to
/// has no tree before it. `read` gives the entries of a tree, of the push or
/// of the repository.
///
/// A directory whose name is refused may be kept where the tree before had it,
/// but nothing under it is brought anew: an entry below it that no tree before
/// had at the same place, by name, type and id, is refused for the name of the
/// directory. A checkout would write it into the directory of Git or of alt.
/// A file, symbolic link or submodule whose name is refused may be kept only
/// as it was, by mode and id.
pub fn first_new<O: Copy + Ord, E>(
    commits: &[Commit<O>],
    flagged: &BTreeMap<O, Flagged<O>>,
    read: &mut Trees<'_, O, E>,
    file: &mut Files<'_, O, E>,
) -> Result<Option<(O, &'static str)>, E> {
    let pending = pending(flagged, file)?;
    if pending.is_empty() {
        return Ok(None);
    }
    let mut come_to = BTreeSet::new();
    let mut seen = BTreeSet::new();
    // each tree with the trees before it, and the fault of the name of the
    // nearest directory above it whose name is refused
    let mut walk: Vec<(O, Vec<O>, Option<&'static str>)> = (commits.iter())
        .map(|commit| (commit.tree, commit.parents.clone(), None))
        .collect();
    while let Some((tree, mut before, below)) = walk.pop() {
        before.sort_unstable();
        before.dedup();
        if before.contains(&tree) || !seen.insert((tree, before.clone(), below)) {
            continue;
        }
        let entries = read(&tree)?;
        let mut trees_before = Vec::with_capacity(before.len());
        for tree in &before {
            trees_before.push(read(tree)?);
        }
        if let Some(pending) = pending.get(&tree) {
            come_to.insert(tree);
            if let Some(refused) = refused(&tree, pending, &trees_before) {
                return Ok(Some(refused));
            }
        }
        if let Some(why) = below
            && entries.iter().any(|entry| !had(entry, &trees_before))
        {
            return Ok(Some((tree, why)));
        }
        for entry in entries.iter().filter(|entry| entry.mode & 0o170000 == TREE) {
            let under = (trees_before.iter().flatten())
                .filter(|was| was.name == entry.name && was.mode & 0o170000 == TREE)
                .map(|was| was.oid);
            let below = below.or_else(|| unsafe_name(&entry.name));
            walk.push((entry.oid, under.collect(), below));
        }
    }
    let alone = pending.iter().filter(|(tree, _)| !come_to.contains(*tree));
    Ok(alone
        .filter_map(|(tree, pending)| refused(tree, pending, &[]))
        .next())
}

/// What of each tree of `flagged` is refused unless the tree before it had
/// it, where anything is: its faults, and the files it names whose blobs
/// `file` tells a fault of.
fn pending<'a, O: Copy + Ord, E>(
    flagged: &'a BTreeMap<O, Flagged<O>>,
    file: &mut Files<'_, O, E>,
) -> Result<BTreeMap<O, Pending<'a, O>>, E> {
    // a file is read once, and only a fault is looked for where it is
    let mut told = BTreeMap::new();
    let mut pending = BTreeMap::new();
    for (tree, found) in flagged {
        let mut files = Vec::new();
        for named in &found.files {
            let why = match told.get(&(named.oid, named.special)) {
                Some(why) => *why,
                None => {
                    let why = file(&named.oid, named.special)?;
                    told.insert((named.oid, named.special), why);
                    why
                }
            };
            files.extend(why.map(|why| (named, why)));
        }
        if !found.faults.is_empty() || !files.is_empty() {
            let faults = found.faults.iter().collect();
            pending.insert(*tree, Pending { faults, files });
        }
    }
    Ok(pending)
}

/// whether one of the trees `before` at the same place had `entry`: by its
/// name, its mode and its id; a directory by its name alone, what is under it
/// being walked
fn had<O: PartialEq>(entry: &Entry<Vec<u8>, O>, before: &[Vec<Entry<Vec<u8>, O>>]) -> bool {
    before
        .iter()
        .flatten()
        .any(|was| match entry.mode & 0o170000 == TREE {
            true => was.name == entry.name && was.mode & 0o170000 == TREE,
            false => was.name == entry.name && was.mode == entry.mode && was.oid == entry.oid,
        })
}

/// what of a tree at a place where the trees `before` were is refused
fn refused<O: Copy + PartialEq>(
    tree: &O,
    pending: &Pending<'_, O>,
    before: &[Vec<Entry<Vec<u8>, O>>],
) -> Option<(O, &'static str)> {
    if let Some(fault) = pending.faults.iter().find(|fault| !fault.had(before)) {
        return Some((*tree, fault.why));
    }
    let new = pending.files.iter().find(|(file, _)| !file.had(before));
    new.map(|(file, why)| (file.oid, *why))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flagged;
    use std::collections::HashMap;

    type Tree = Vec<Entry<Vec<u8>, u8>>;

    fn entry(name: &str, mode: u32, oid: u8) -> Entry<Vec<u8>, u8> {
        let name = name.as_bytes().to_vec();
        Entry { name, mode, oid }
    }

    /// what is refused of the trees `trees` that commits of `commits` bring
    fn first(
        trees: &HashMap<u8, Tree>,
        new: &[u8],
        commits: &[(u8, Vec<u8>)],
        bad_blobs: &[u8],
    ) -> Option<(u8, &'static str)> {
        let flagged: BTreeMap<u8, Flagged<u8>> = (new.iter())
            .filter_map(|tree| Some((*tree, flagged(&trees[tree])?)))
            .collect();
        let commits: Vec<Commit<u8>> = (commits.iter())
            .map(|(tree, parents)| Commit {
                tree: *tree,
                parents: parents.clone(),
            })
            .collect();
        let mut read = |tree: &u8| Ok::<_, ()>(trees[tree].clone());
        let mut file = |blob: &u8, _| Ok(bad_blobs.contains(blob).then_some("bad"));
        first_new(&commits, &flagged, &mut read, &mut file).unwrap()
    }

    #[test]
    fn what_a_tree_before_had_at_the_same_place_is_not_refused_and_what_is_new_is() {
        let file = 0o100644;
        let mut trees = HashMap::new();
        // the root had `.alt` and a directory below it with `GIT~1`
        trees.insert(1, vec![entry(".alt", file, 50), entry("d", TREE, 2)]);
        trees.insert(2, vec![entry("GIT~1", file, 51)]);
        // a new root that changes a file: the old names come again
        trees.insert(3, vec![entry(".alt", file, 50), entry("d", TREE, 4)]);
        trees.insert(4, vec![entry("GIT~1", file, 51), entry("x", file, 52)]);
        assert_eq!(first(&trees, &[3, 4], &[(3, vec![1])], &[]), None);
        // the same trees without a parent: every name is new
        let alt = Some((3, "hasDotalt: contains '.alt'"));
        assert_eq!(first(&trees, &[3, 4], &[(3, vec![])], &[]), alt);
        // `GIT~1` moved to another directory: new at that place
        trees.insert(5, vec![entry(".alt", file, 50), entry("e", TREE, 4)]);
        let git = Some((4, "hasDotgit: contains '.git'"));
        assert_eq!(first(&trees, &[5, 4], &[(5, vec![1])], &[]), git);
        // a tree no commit brings has nothing before it
        assert_eq!(first(&trees, &[4], &[], &[]), git);
        // a merge: a name one parent had
        trees.insert(6, vec![entry("a", file, 53)]);
        assert_eq!(first(&trees, &[3, 4], &[(3, vec![6, 1])], &[]), None);

        // `.gitmodules` whose blob is refused: where it is the blob the tree
        // before named there it is not, where it is another it is
        trees.insert(10, vec![entry(".gitmodules", file, 60)]);
        trees.insert(
            11,
            vec![entry(".gitmodules", file, 60), entry("y", file, 61)],
        );
        trees.insert(12, vec![entry(".gitmodules", file, 62)]);
        assert_eq!(first(&trees, &[11], &[(11, vec![10])], &[60]), None);
        assert_eq!(
            first(&trees, &[11], &[(11, vec![])], &[60]),
            Some((60, "bad"))
        );
        assert_eq!(
            first(&trees, &[12], &[(12, vec![10])], &[62]),
            Some((62, "bad"))
        );
        // a blob nothing is wrong with is no fault anywhere, and no tree is walked
        let mut none = HashMap::new();
        none.insert(12, trees[&12].clone());
        assert_eq!(first(&none, &[12], &[(12, vec![99])], &[]), None);
    }

    #[test]
    fn nothing_is_brought_anew_under_a_directory_whose_name_is_refused_where_it_is_kept() {
        let (file, run) = (0o100644, 0o100755);
        let alt = Some("hasDotalt: contains '.alt'");
        let mut trees = HashMap::new();
        // the root had `ALT~1`, a directory of `config` and a directory `d` of `x`
        trees.insert(1, vec![entry("ALT~1", TREE, 2), entry("a", file, 50)]);
        trees.insert(2, vec![entry("config", file, 51), entry("d", TREE, 3)]);
        trees.insert(3, vec![entry("x", file, 52)]);
        let root = |under: u8| vec![entry("ALT~1", TREE, under), entry("a", file, 53)];
        // kept as it was, another file beside it: taken
        trees.insert(10, root(2));
        assert_eq!(first(&trees, &[10], &[(10, vec![1])], &[]), None);
        // a file new under it
        trees.insert(11, root(12));
        trees.insert(
            12,
            vec![entry("config", file, 51), entry("hooks", file, 54)],
        );
        let refused = |at: u8| alt.map(|why| (at, why));
        assert_eq!(first(&trees, &[11], &[(11, vec![1])], &[]), refused(12));
        // a file under it of other bytes, or of another mode
        trees.insert(13, root(14));
        trees.insert(14, vec![entry("config", file, 55), entry("d", TREE, 3)]);
        assert_eq!(first(&trees, &[13], &[(13, vec![1])], &[]), refused(14));
        trees.insert(15, root(16));
        trees.insert(16, vec![entry("config", run, 51), entry("d", TREE, 3)]);
        assert_eq!(first(&trees, &[15], &[(15, vec![1])], &[]), refused(16));
        // a file new in a directory below it
        trees.insert(17, root(18));
        trees.insert(18, vec![entry("config", file, 51), entry("d", TREE, 19)]);
        trees.insert(19, vec![entry("x", file, 52), entry("y", file, 56)]);
        assert_eq!(first(&trees, &[17], &[(17, vec![1])], &[]), refused(19));
        // what was under it, less: taken
        trees.insert(20, root(21));
        trees.insert(21, vec![entry("config", file, 51)]);
        assert_eq!(first(&trees, &[20], &[(20, vec![1])], &[]), None);
        // a directory of the repository brought to where a file was
        trees.insert(22, root(23));
        trees.insert(23, vec![entry("config", file, 51), entry("d", TREE, 2)]);
        assert_eq!(first(&trees, &[22], &[(22, vec![1])], &[]), refused(2));
    }

    #[test]
    fn a_file_a_symbolic_link_or_a_submodule_whose_name_is_refused_is_kept_only_as_it_was() {
        let (file, run, link, module) = (0o100644, 0o100755, 0o120000, 0o160000);
        let mut trees = HashMap::new();
        // `sub/.GIT` a file, `ALT~1` a file, `.ALT` a symbolic link, `GIT~1` a submodule
        trees.insert(1, vec![entry("sub", TREE, 2), entry("ALT~1", file, 50)]);
        trees.insert(2, vec![entry(".GIT", file, 51)]);
        trees.insert(3, vec![entry(".ALT", link, 52), entry("GIT~1", module, 53)]);
        let git = |at| Some((at, "hasDotgit: contains '.git'"));
        let alt = |at| Some((at, "hasDotalt: contains '.alt'"));
        // kept as they were, another file beside them: taken
        trees.insert(10, vec![entry("sub", TREE, 11), entry("ALT~1", file, 50)]);
        trees.insert(11, vec![entry(".GIT", file, 51), entry("a", file, 54)]);
        assert_eq!(first(&trees, &[10, 11], &[(10, vec![1])], &[]), None);
        // other bytes under `sub/.GIT`
        trees.insert(12, vec![entry("sub", TREE, 13), entry("ALT~1", file, 50)]);
        trees.insert(13, vec![entry(".GIT", file, 55)]);
        assert_eq!(first(&trees, &[12, 13], &[(12, vec![1])], &[]), git(13));
        // `ALT~1` made to run, and of other bytes
        trees.insert(14, vec![entry("sub", TREE, 2), entry("ALT~1", run, 50)]);
        assert_eq!(first(&trees, &[14], &[(14, vec![1])], &[]), alt(14));
        trees.insert(15, vec![entry("sub", TREE, 2), entry("ALT~1", file, 56)]);
        assert_eq!(first(&trees, &[15], &[(15, vec![1])], &[]), alt(15));
        // `.ALT` to another target, `GIT~1` at another commit
        trees.insert(
            16,
            vec![entry(".ALT", link, 57), entry("GIT~1", module, 53)],
        );
        assert_eq!(first(&trees, &[16], &[(16, vec![3])], &[]), alt(16));
        trees.insert(
            17,
            vec![entry(".ALT", link, 52), entry("GIT~1", module, 58)],
        );
        assert_eq!(first(&trees, &[17], &[(17, vec![3])], &[]), git(17));
        trees.insert(
            18,
            vec![entry(".ALT", link, 52), entry("GIT~1", module, 53)],
        );
        assert_eq!(first(&trees, &[18], &[(18, vec![3])], &[]), None);
    }
}
