mod history;
mod reached;
#[cfg(test)]
use history::CommitSummary;
pub use history::history;

use crate::web_route::Within;
use alt_git_codec::{Commit, HashAlgo, ObjectId, ObjectKind, Tag, Tree};
use alt_server_store::{Credential, Error, Result, Store};
use std::io::Read;

const STRUCTURED: usize = 8 * 1024 * 1024;
const PREVIEW: usize = 512 * 1024;
const WORK: u64 = 64 * 1024 * 1024;
/// what an object is refused with that is larger than is left to read
const READ: &str = "structured preview bytes";
/// the entries of a directory in one page
const ENTRIES: usize = 500;
struct Reader<'a> {
    store: &'a mut Store,
    credential: &'a Credential,
    repository: &'a str,
    remaining: u64,
}
impl Reader<'_> {
    fn object(&mut self, oid: ObjectId) -> Result<(ObjectKind, Vec<u8>)> {
        let (kind, size, file) =
            self.store
                .open_published_object(self.credential, self.repository, oid)?;
        if size > STRUCTURED as u64 || size > self.remaining {
            return Err(Error::Limit(READ));
        }
        self.remaining -= size;
        let mut raw = Vec::new();
        file.take(size + 1).read_to_end(&mut raw)?;
        if raw.len() as u64 != size || ObjectId::hash_object(HashAlgo::Sha1, kind, &raw) != oid {
            return Err(Error::Integrity("preview object identity"));
        }
        Ok((kind, raw))
    }
    /// What is at `path` in the tree of `commit`, with the commit a tag was
    /// followed to and whether it is a directory; the tree itself where the
    /// path is empty.
    fn at(&mut self, commit: ObjectId, path: &[u8]) -> Result<(ObjectId, ObjectId, bool)> {
        let (commit_id, commit) = self.commit(commit)?;
        let mut oid = commit.tree().ok_or(Error::Integrity("commit tree"))?;
        let mut directory = true;
        if !path.is_empty() {
            for component in path.split(|b| *b == b'/') {
                if !directory {
                    return Err(Error::Precondition("tree path is not a directory"));
                }
                let tree = self.tree(oid)?;
                let entry = tree
                    .entries
                    .into_iter()
                    .find(|e| e.name.as_slice() == component)
                    .ok_or(Error::Precondition("tree path absent"))?;
                // a link names a commit of another repository: whatever of this
                // one has that id is not at this path
                if entry.mode.is_gitlink() {
                    return Err(Error::Precondition("tree path is a link"));
                }
                directory = entry.mode.is_tree();
                oid = entry.oid;
            }
        }
        Ok((commit_id, oid, directory))
    }
    fn commit(&mut self, mut oid: ObjectId) -> Result<(ObjectId, Commit)> {
        for _ in 0..8 {
            let (kind, raw) = self.object(oid)?;
            match kind {
                ObjectKind::Commit => {
                    return Ok((
                        oid,
                        Commit::parse(&raw).map_err(|_| Error::Integrity("commit preview"))?,
                    ));
                }
                ObjectKind::Tag => {
                    oid = Tag::parse(&raw)
                        .map_err(|_| Error::Integrity("tag preview"))?
                        .object()
                        .ok_or(Error::Integrity("tag target"))?
                }
                _ => return Err(Error::Precondition("selected object is not a commit")),
            }
        }
        Err(Error::Limit("tag preview depth"))
    }
    fn tree(&mut self, oid: ObjectId) -> Result<Tree> {
        let (kind, raw) = self.object(oid)?;
        if kind != ObjectKind::Tree {
            return Err(Error::Precondition("selected object is not a tree"));
        }
        Tree::parse(&raw, HashAlgo::Sha1).map_err(|_| Error::Integrity("tree preview"))
    }
}
#[derive(alt_json::ToJson)]
pub struct TreePage {
    commit: String,
    path: String,
    parent: Option<String>,
    display_path: String,
    entries: Vec<TreeEntry>,
    /// where the page after this one begins, when this is not the last
    next: Option<String>,
}
#[derive(alt_json::ToJson)]
struct TreeEntry {
    name: String,
    path: String,
    display_path: String,
    oid: String,
    kind: &'static str,
    bytes: Option<u64>,
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn path_bytes(value: &str) -> Result<Vec<u8>> {
    if value.len() > 8192
        || !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Precondition("encoded tree path"));
    }
    let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
    let path = value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| digit(p[0]) * 16 + digit(p[1]))
        .collect::<Vec<_>>();
    if !path.is_empty()
        && path
            .split(|b| *b == b'/')
            .any(|p| p.is_empty() || p == b"." || p == b".." || p.contains(&0))
    {
        return Err(Error::Precondition("tree path component"));
    }
    Ok(path)
}
fn display(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        text.chars()
            .flat_map(|c| {
                if c.is_control() || c == '\\' {
                    c.escape_default().collect::<Vec<_>>()
                } else {
                    vec![c]
                }
            })
            .collect()
    } else {
        bytes
            .iter()
            .map(|b| {
                if (32..127).contains(b) && *b != b'\\' {
                    char::from(*b).to_string()
                } else {
                    format!("\\x{b:02x}")
                }
            })
            .collect()
    }
}
/// A page of `tree`, which is at `raw_path`: up to `limit` entries from the one
/// at `after` on, and where the next page begins when there is one. Each entry
/// carries its whole path twice, so what a directory is listed in grows with the
/// two multiplied: the page ends before the entry that would take what is written
/// of it past `bound`. The first entry of a page is written whatever it takes, so
/// every page goes on.
fn listed(
    raw_path: &[u8],
    tree: Tree,
    after: usize,
    limit: usize,
    bound: usize,
) -> Result<(Vec<TreeEntry>, Option<usize>)> {
    if after > tree.entries.len() {
        return Err(Error::Precondition("tree page"));
    }
    // the brackets around the entries, and for each the comma after it
    let mut written = 2;
    let mut entries = Vec::new();
    for (index, entry) in tree.entries.into_iter().enumerate().skip(after) {
        let mut path = raw_path.to_vec();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(&entry.name);
        let entry = TreeEntry {
            name: display(&entry.name),
            path: hex(&path),
            display_path: display(&path),
            oid: entry.oid.to_string(),
            kind: if entry.mode.is_tree() {
                "tree"
            } else if entry.mode.is_gitlink() {
                "gitlink"
            } else if entry.mode.is_symlink() {
                "symlink"
            } else {
                "blob"
            },
            bytes: None,
        };
        written += alt_json::to_vec(&entry)?.len() + 1;
        if entries.len() == limit || (written > bound && !entries.is_empty()) {
            return Ok((entries, Some(index)));
        }
        entries.push(entry);
    }
    Ok((entries, None))
}
pub fn tree(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    (commit, gone): (ObjectId, Option<&std::sync::atomic::AtomicBool>),
    path: &str,
    after: usize,
) -> Result<TreePage> {
    let raw_path = path_bytes(path)?;
    reached::led_to(store, credential, repository, (commit, gone))?;
    let mut reader = Reader {
        store,
        credential,
        repository,
        remaining: WORK,
    };
    let (commit_id, oid, directory) = reader.at(commit, &raw_path)?;
    if !directory {
        return Err(Error::Precondition("tree path is not a directory"));
    }
    let (entries, next) = listed(&raw_path, reader.tree(oid)?, after, ENTRIES, STRUCTURED)?;
    let parent = if raw_path.is_empty() {
        None
    } else {
        Some(hex(raw_path
            .iter()
            .rposition(|b| *b == b'/')
            .map_or(&[][..], |position| &raw_path[..position])))
    };
    Ok(TreePage {
        commit: commit_id.to_string(),
        path: path.into(),
        parent,
        display_path: display(&raw_path),
        entries,
        next: next.map(|index| index.to_string()),
    })
}
#[derive(alt_json::ToJson)]
pub struct BlobPreview {
    oid: String,
    bytes: String,
    text: Option<String>,
    truncated: bool,
    download_url: String,
}
/// Holds `oid` to what the caller is shown: the commit `within` names, which a
/// ref the caller reads leads to, or what is at the path `within` names in the
/// tree of that commit. An object that is in the segments and is shown nowhere
/// the caller reads, of a branch that was deleted or of history that was
/// replaced, is not given to whoever knows its id.
pub fn shown(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    (oid, gone): (ObjectId, Option<&std::sync::atomic::AtomicBool>),
    within: &Within,
) -> Result<()> {
    let raw_path = path_bytes(&within.path)?;
    reached::led_to(store, credential, repository, (within.commit, gone))?;
    let mut reader = Reader {
        store,
        credential,
        repository,
        remaining: WORK,
    };
    let (commit, at, _) = reader.at(within.commit, &raw_path)?;
    match oid == at || (raw_path.is_empty() && oid == commit) {
        true => Ok(()),
        false => Err(Error::Unauthorized),
    }
}
pub fn blob(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    (oid, gone): (ObjectId, Option<&std::sync::atomic::AtomicBool>),
    within: &Within,
) -> Result<BlobPreview> {
    shown(store, credential, repository, (oid, gone), within)?;
    let (kind, size, file) = store.open_published_object(credential, repository, oid)?;
    if kind != ObjectKind::Blob {
        return Err(Error::Precondition("selected object is not a blob"));
    }
    let mut prefix = Vec::new();
    file.take(PREVIEW as u64).read_to_end(&mut prefix)?;
    let truncated = size > PREVIEW as u64;
    let text = if prefix.contains(&0) {
        None
    } else {
        match std::str::from_utf8(&prefix) {
            Ok(text) => Some(text.to_owned()),
            Err(error) if truncated && error.error_len().is_none() => Some(
                std::str::from_utf8(&prefix[..error.valid_up_to()])
                    .map_err(|_| Error::Integrity("text prefix"))?
                    .into(),
            ),
            Err(_) => None,
        }
    };
    Ok(BlobPreview {
        oid: oid.to_string(),
        bytes: size.to_string(),
        text,
        truncated,
        download_url: format!(
            "/web/repositories/{repository}/objects/{oid}/download?commit={}&path={}",
            within.commit, within.path
        ),
    })
}
#[cfg(test)]
mod json_tests;
#[cfg(test)]
mod tests;
