//! Whether the blobs a tree about to be committed names are stored.
//!
//! A tree is built from the index, which names blobs by id and is written
//! with its own fsync: after a power cut it can name a blob whose content
//! never arrived. Nothing in building the tree reads the blobs.

use std::collections::HashMap;

use alt_bytes::{BString, ByteSlice};
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind, Tree};
use alt_odb::NativeOdb;

use crate::WorktreeError;

fn entries(odb: &NativeOdb, tree: ObjectId, algo: HashAlgo) -> Result<Tree, WorktreeError> {
    let obj = odb.get(&tree)?.ok_or(WorktreeError::NotATree(tree))?;
    if obj.kind != ObjectKind::Tree {
        return Err(WorktreeError::NotATree(tree));
    }
    Ok(Tree::parse(&obj.data, algo)?)
}

/// The first file under `tree`, by path, whose blob the store does not
/// hold, among the files `base` does not have with the same blob: what a
/// tree that is stored and committed names is taken to be there, so only
/// the directories that differ from `base` are read.
pub fn unstored_blob(
    odb: &NativeOdb,
    tree: ObjectId,
    base: Option<ObjectId>,
    algo: HashAlgo,
) -> Result<Option<(BString, ObjectId)>, WorktreeError> {
    unstored_under(odb, tree, base, algo, &mut Vec::new())
}

fn unstored_under(
    odb: &NativeOdb,
    tree: ObjectId,
    base: Option<ObjectId>,
    algo: HashAlgo,
    prefix: &mut Vec<u8>,
) -> Result<Option<(BString, ObjectId)>, WorktreeError> {
    if base == Some(tree) {
        return Ok(None);
    }
    let was: HashMap<BString, (ObjectId, bool)> = match base {
        Some(base) => entries(odb, base, algo)?
            .entries
            .into_iter()
            .map(|e| (e.name, (e.oid, e.mode.is_tree())))
            .collect(),
        None => HashMap::new(),
    };
    for entry in entries(odb, tree, algo)?.entries {
        let before = was.get(&entry.name);
        if before.is_some_and(|(oid, _)| *oid == entry.oid) || entry.mode.is_gitlink() {
            continue;
        }
        let mark = prefix.len();
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        prefix.extend_from_slice(entry.name.as_bytes());
        if entry.mode.is_tree() {
            let base = before.filter(|(_, is_tree)| *is_tree).map(|(oid, _)| *oid);
            if let Some(found) = unstored_under(odb, entry.oid, base, algo, prefix)? {
                return Ok(Some(found));
            }
        } else if !odb.contains(&entry.oid)? {
            return Ok(Some((BString::from(prefix.clone()), entry.oid)));
        }
        prefix.truncate(mark);
    }
    Ok(None)
}
