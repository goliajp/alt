//! What `git fsck` refuses in the trees and blobs of a pack, and which of it
//! a push brings anew: a name a checkout would write outside the directory of
//! the tree, into the directory of Git or of alt on some file system, or
//! twice, `.gitmodules` as a symbolic link, and the blobs trees name
//! `.gitmodules` or `.gitattributes` that Git refuses. A tree that only is not
//! written as Git writes it, out of order or with a mode given zeros before
//! it, is taken as it is.

use crate::{Error, Ingested, Kind, Oid, SegmentSet};
use alt_git_fsck::{ATTRIBUTES_LARGEST, Blob, Entry, Flagged, MODULES_LARGEST, Special};
use std::collections::BTreeMap;

/// The entries of tree `data`, each its name, its mode and the id it names;
/// none where the tree is not read so far.
fn entries(data: &[u8]) -> Option<Vec<Entry<&[u8], Oid>>> {
    let mut entries = Vec::new();
    let mut rest = data;
    while let Some(space) = rest.iter().position(|byte| *byte == b' ') {
        let mode = rest[..space].iter().try_fold(0u64, |mode, digit| {
            let digit = (b'0'..=b'7')
                .contains(digit)
                .then(|| u64::from(digit - b'0'))?;
            Some((mode << 3 | digit).min(u64::from(u32::MAX)))
        })? as u32;
        rest = &rest[space + 1..];
        let nul = rest.iter().position(|byte| *byte == 0)?;
        let oid = rest
            .get(nul + 1..nul + 21)?
            .try_into()
            .expect("twenty bytes");
        entries.push(Entry {
            name: &rest[..nul],
            mode,
            oid,
        });
        rest = &rest[nul + 21..];
    }
    Some(entries)
}

/// What `git fsck` looks at again of tree `data`, where it does at anything.
pub(crate) fn flagged(data: &[u8]) -> Option<Flagged<Oid>> {
    alt_git_fsck::flagged(&entries(data)?)
}

/// What a push is refused for in tree `data`, of what is in the tree itself:
/// an entry `git fsck` refuses it for, or a mode of more digits than a reader
/// of trees reads.
pub(crate) fn refused(data: &[u8]) -> Option<&'static str> {
    let mut rest = data;
    while let Some(space) = rest.iter().position(|byte| *byte == b' ') {
        if space > 16 {
            return Some("badMode: contains bad file modes");
        }
        let nul = rest.iter().position(|byte| *byte == 0)?;
        rest = rest.get(nul + 21..)?;
    }
    flagged(data)?.faults.first().map(|fault| fault.why)
}

/// Fails with what `git fsck` refuses that the pack of `ingested` brings
/// anew, published as a segment of `set` behind the segments `before`: a
/// fault of one of its trees, or of a file one of them names, that a tree at
/// the same place in a parent of its commit did not have. What `before`
/// already held is no tree of the push. The trees are walked within `steps`.
pub fn refused_anew(
    ingested: &Ingested,
    (before, set): (&SegmentSet, &SegmentSet),
    steps: usize,
) -> Result<(), Error> {
    let flagged: BTreeMap<Oid, Flagged<Oid>> = (ingested.flagged.iter())
        .filter(|(tree, _)| !before.locate_any(tree))
        .cloned()
        .collect();
    if flagged.is_empty() {
        return Ok(());
    }
    let tree_of = |commit: &Oid| set.commit(commit).map(|commit| commit.tree);
    let commits: Vec<alt_git_fsck::Commit<Oid>> = (ingested.commits.iter())
        .map(|commit| alt_git_fsck::Commit {
            tree: commit.tree,
            parents: commit.parents.iter().filter_map(tree_of).collect(),
        })
        .collect();
    let mut left = steps;
    let mut read = |tree: &Oid| {
        left = left.checked_sub(1).ok_or(Error::Limit("tree walk steps"))?;
        let (kind, data) = set.read(tree)?;
        let entries = (kind == Kind::Tree)
            .then(|| entries(&data))
            .flatten()
            .ok_or(Error::Corrupt("tree entry"))?;
        let owned = entries.into_iter().map(|entry| Entry {
            name: entry.name.to_vec(),
            mode: entry.mode,
            oid: entry.oid,
        });
        Ok(owned.collect())
    };
    let mut file = |blob: &Oid, special| {
        let largest = match special {
            Special::Gitmodules => MODULES_LARGEST,
            Special::Gitattributes => ATTRIBUTES_LARGEST,
        };
        let data;
        let found = match set.record(blob)? {
            None => Blob::Missing,
            Some(record) if record.kind != Kind::Blob => Blob::NotBlob,
            Some(record) if record.size > largest => Blob::Larger,
            Some(_) => {
                data = set.read(blob)?.1;
                Blob::Bytes(&data)
            }
        };
        Ok(alt_git_fsck::blob_fault(special, found))
    };
    match alt_git_fsck::first_new::<Oid, Error>(&commits, &flagged, &mut read, &mut file)? {
        Some((oid, why)) => Err(Error::Unsafe(oid, why)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(names: &[&[u8]]) -> Vec<u8> {
        let mut data = Vec::new();
        for name in names {
            data.extend_from_slice(b"100644 ");
            data.extend_from_slice(name);
            data.push(0);
            data.extend_from_slice(&[7; 20]);
        }
        data
    }

    #[test]
    fn what_a_push_is_refused_for_in_a_tree_itself() {
        let said = |name: &[u8]| refused(&tree(&[b"a", name]));
        assert_eq!(said(b".GIT"), Some("hasDotgit: contains '.git'"));
        assert_eq!(said(b"alt~1"), Some("hasDotalt: contains '.alt'"));
        assert_eq!(said(b".."), Some("hasDotdot: contains '..'"));
        assert_eq!(said(b".gitignore"), None);
        let twice = Some("duplicateEntries: contains duplicate file entries");
        assert_eq!(refused(&tree(&[b"b", b"a", b"b"])), twice);
        // out of order, and a mode with a zero before it
        assert_eq!(refused(&tree(&[b"b", b"a"])), None);
        let mut padded = b"040000 d\0".to_vec();
        padded.extend_from_slice(&[7; 20]);
        assert_eq!(refused(&padded), None);
        let mut link = b"120000 .gitmodules\0".to_vec();
        link.extend_from_slice(&[7; 20]);
        assert_eq!(
            refused(&link),
            Some("gitmodulesSymlink: .gitmodules is a symbolic link")
        );
        // a mode too long to be read
        let mut long = b"00000000000100644 f\0".to_vec();
        long.extend_from_slice(&[7; 20]);
        assert_eq!(
            refused(&[&padded[..], &long].concat()),
            Some("badMode: contains bad file modes")
        );
        assert_eq!(refused(&long[1..]), None);
        // a `.gitattributes` is looked at again, for its blob
        let files = flagged(&tree(&[b".gitattributes"])).unwrap().files;
        assert_eq!(files[0].special, Special::Gitattributes);
    }
}
