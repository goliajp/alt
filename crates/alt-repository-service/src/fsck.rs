//! What `git fsck` refuses that the objects of a publication bring anew: a
//! name of a tree, or the blob of a `.gitmodules` or `.gitattributes`, that a
//! tree at the same place in a parent of its commit did not have. What the
//! repository already holds is not refused again.

use crate::object::verify_object;
use crate::{Error, GraphLimits, Node, ObjectSource, Result};
use alt_git_codec::{ObjectId, ObjectKind};
use alt_git_fsck::{ATTRIBUTES_LARGEST, Blob, Commit, Entry, Flagged, MODULES_LARGEST, Special};
use std::collections::BTreeMap;
use std::io::Read;

/// Fails with what `git fsck` says of the first fault `nodes`, the objects
/// read for a publication, bring anew. Objects beyond them are read from
/// `source`; the trees are walked within the steps of `limits`.
pub(crate) fn refused_anew(
    source: &impl ObjectSource,
    nodes: &BTreeMap<ObjectId, Node>,
    limits: GraphLimits,
) -> Result<()> {
    let mut flagged = BTreeMap::new();
    for (oid, node) in nodes {
        if let Node::Tree(links) = node {
            let entries: Vec<Entry<&[u8], ObjectId>> = (links.iter())
                .map(|link| Entry {
                    name: &link.name[..],
                    mode: link.mode,
                    oid: link.oid,
                })
                .collect();
            flagged.extend(alt_git_fsck::flagged(&entries).map(|found| (*oid, found)));
        }
    }
    let flagged: BTreeMap<ObjectId, Flagged<ObjectId>> = flagged;
    if flagged.is_empty() {
        return Ok(());
    }
    let node = |oid: &ObjectId| -> Result<Option<Node>> {
        if let Some(node) = nodes.get(oid) {
            return Ok(Some(node.clone()));
        }
        let Some(object) = source.open(*oid)? else {
            return Ok(None);
        };
        verify_object(*oid, object, limits).map(Some)
    };
    let mut commits = Vec::new();
    for node_of in nodes.values() {
        if let Node::Commit { tree, parents } = node_of {
            let mut before = Vec::new();
            for parent in parents {
                if let Some(Node::Commit { tree, .. }) = node(parent)? {
                    before.push(tree);
                }
            }
            let (tree, parents) = (*tree, before);
            commits.push(Commit { tree, parents });
        }
    }
    let mut left = limits.walk_steps;
    let mut read = |tree: &ObjectId| -> Result<Vec<Entry<Vec<u8>, ObjectId>>> {
        left = left
            .checked_sub(1)
            .ok_or(Error::Limit("graph traversal work"))?;
        let Some(Node::Tree(links)) = node(tree)? else {
            return Err(Error::Integrity("dependency type mismatch"));
        };
        let entries = links.into_iter().map(|link| Entry {
            name: link.name,
            mode: link.mode,
            oid: link.oid,
        });
        Ok(entries.collect())
    };
    let mut file = |blob: &ObjectId, special| -> Result<Option<&'static str>> {
        let largest = match special {
            Special::Gitmodules => MODULES_LARGEST,
            Special::Gitattributes => ATTRIBUTES_LARGEST,
        };
        let mut data = Vec::new();
        let found = match source.open(*blob)? {
            None => Blob::Missing,
            Some(object) if object.kind != ObjectKind::Blob => Blob::NotBlob,
            Some(object) if object.bytes > largest => Blob::Larger,
            Some(object) => {
                object.reader.take(largest).read_to_end(&mut data)?;
                Blob::Bytes(&data)
            }
        };
        Ok(alt_git_fsck::blob_fault(special, found))
    };
    match alt_git_fsck::first_new(&commits, &flagged, &mut read, &mut file)? {
        Some((_, why)) => Err(Error::Integrity(why)),
        None => Ok(()),
    }
}
