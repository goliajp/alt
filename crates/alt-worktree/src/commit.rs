//! Commit objects, written as git writes them.

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

use crate::WorktreeError;

/// A commit author/committer line: `Name <email> <unix-ts> <tz>`.
pub struct Sig<'a> {
    pub name: &'a str,
    pub email: &'a str,
    /// Seconds since the epoch.
    pub when: i64,
    /// Timezone like `+0000`.
    pub tz: &'a str,
}

/// Writes a commit object and returns its id. The bytes are git-canonical,
/// so the id equals `git commit-tree`'s for the same inputs.
pub fn write_commit(
    odb: &mut NativeOdb,
    tree: ObjectId,
    parents: &[ObjectId],
    author: &Sig,
    committer: &Sig,
    message: &str,
    algo: HashAlgo,
) -> Result<ObjectId, WorktreeError> {
    let bytes = build_commit_bytes(tree, parents, author, committer, message);
    let id = ObjectId::hash_object(algo, ObjectKind::Commit, &bytes);
    odb.put(id, ObjectKind::Commit, &bytes)?;
    Ok(id)
}

/// Just the byte assembly half of [`write_commit`] — useful for callers
/// (e.g. commit signing) that want to mutate the bytes before
/// hashing + storing. The caller is responsible for the put + hash.
pub fn build_commit_bytes(
    tree: ObjectId,
    parents: &[ObjectId],
    author: &Sig,
    committer: &Sig,
    message: &str,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("tree {tree}\n").as_bytes());
    for p in parents {
        bytes.extend_from_slice(format!("parent {p}\n").as_bytes());
    }
    let line = |s: &Sig| format!("{} <{}> {} {}", s.name, s.email, s.when, s.tz);
    bytes.extend_from_slice(format!("author {}\n", line(author)).as_bytes());
    bytes.extend_from_slice(format!("committer {}\n", line(committer)).as_bytes());
    bytes.push(b'\n');
    bytes.extend_from_slice(message.as_bytes());
    bytes
}
