use alt_git_codec::{Commit, HashAlgo, ObjectId, ObjectKind, Tag, Tree};
use alt_server_store::{Credential, Error, Page, Result, Store};
use serde::Serialize;
use std::io::Read;

const STRUCTURED: usize = 8 * 1024 * 1024;
const PREVIEW: usize = 512 * 1024;
const WORK: u64 = 64 * 1024 * 1024;
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
            return Err(Error::Limit("structured preview bytes"));
        }
        self.remaining -= size;
        let mut raw = Vec::new();
        file.take(size + 1).read_to_end(&mut raw)?;
        if raw.len() as u64 != size || ObjectId::hash_object(HashAlgo::Sha1, kind, &raw) != oid {
            return Err(Error::Integrity("preview object identity"));
        }
        Ok((kind, raw))
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
#[derive(Serialize, alt_json::ToJson)]
pub struct TreePage {
    commit: String,
    path: String,
    parent: Option<String>,
    display_path: String,
    entries: Vec<TreeEntry>,
}
#[derive(Serialize, alt_json::ToJson)]
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
pub fn tree(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    commit: ObjectId,
    path: &str,
) -> Result<TreePage> {
    let raw_path = path_bytes(path)?;
    let mut reader = Reader {
        store,
        credential,
        repository,
        remaining: WORK,
    };
    let (commit_id, commit) = reader.commit(commit)?;
    let mut oid = commit.tree().ok_or(Error::Integrity("commit tree"))?;
    if !raw_path.is_empty() {
        for component in raw_path.split(|b| *b == b'/') {
            let tree = reader.tree(oid)?;
            let entry = tree
                .entries
                .into_iter()
                .find(|e| e.name.as_slice() == component)
                .ok_or(Error::Precondition("tree path absent"))?;
            if !entry.mode.is_tree() {
                return Err(Error::Precondition("tree path is not a directory"));
            }
            oid = entry.oid;
        }
    }
    let tree = reader.tree(oid)?;
    if tree.entries.len() > 10_000 {
        return Err(Error::Limit("tree preview entries"));
    }
    let entries = tree
        .entries
        .into_iter()
        .map(|entry| {
            let mut path = raw_path.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(&entry.name);
            TreeEntry {
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
            }
        })
        .collect();
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
    })
}
#[derive(Serialize, alt_json::ToJson)]
pub struct BlobPreview {
    oid: String,
    bytes: String,
    text: Option<String>,
    truncated: bool,
    download_url: String,
}
pub fn blob(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    oid: ObjectId,
) -> Result<BlobPreview> {
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
        download_url: format!("/web/repositories/{repository}/objects/{oid}/download"),
    })
}
#[derive(Serialize, alt_json::ToJson)]
pub struct CommitSummary {
    oid: String,
    parents: Vec<String>,
    author: String,
    timestamp: String,
    message: String,
}
/// First-parent history. The projection includes all parents so the caller can
/// explicitly inspect other ancestry; it never hides this traversal choice.
pub fn history(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    after: Option<ObjectId>,
    limit: usize,
) -> Result<Page<CommitSummary>> {
    if limit == 0 || limit > 100 {
        return Err(Error::Limit("history page size"));
    }
    let info = store.repository_info(credential, repository)?;
    let start = match after {
        Some(oid) => Some(oid),
        None => info
            .refs
            .get(&info.default_branch)
            .map(|id| id.parse().map_err(|_| Error::Integrity("default ref")))
            .transpose()?,
    };
    let Some(start) = start else {
        return Ok(Page {
            items: vec![],
            next: None,
        });
    };
    let mut reader = Reader {
        store,
        credential,
        repository,
        remaining: WORK,
    };
    let mut pending = Some(start);
    let mut items = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    while items.len() < limit {
        let Some(oid) = pending else {
            break;
        };
        if !seen.insert(oid) {
            return Err(Error::Integrity("commit cycle"));
        }
        let (oid, commit) = reader.commit(oid)?;
        let parents = commit.parents().collect::<Vec<_>>();
        pending = parents.first().copied();
        items.push(CommitSummary {
            oid: oid.to_string(),
            parents: parents.iter().map(ToString::to_string).collect(),
            author: commit.author().map(|v| display(v)).unwrap_or_default(),
            timestamp: commit
                .committer_date()
                .map(|v| v.to_string())
                .unwrap_or_default(),
            message: String::from_utf8_lossy(commit.message()).into_owned(),
        });
    }
    Ok(Page {
        items,
        next: pending.map(|v| v.to_string()),
    })
}

#[cfg(test)]
mod json_tests {
    use alt_json_testkit::fixtures::{check_recorded, written_line_as_serde};
    use alt_json_testkit::rng::Rng;
    use alt_json_testkit::support::Arbitrary;

    use super::*;

    fn any<T: Arbitrary>(rng: &mut Rng) -> T {
        T::arbitrary(rng)
    }

    #[test]
    fn responses_are_written_as_recorded() {
        let mut rng = Rng::new(0xd700);
        let rng = &mut rng;
        let mut lines = String::new();
        for _ in 0..40 {
            lines += &written_line_as_serde(&TreePage {
                commit: any(rng),
                path: any(rng),
                parent: any(rng),
                display_path: any(rng),
                entries: (0..rng.below(3))
                    .map(|_| TreeEntry {
                        name: any(rng),
                        path: any(rng),
                        display_path: any(rng),
                        oid: any(rng),
                        kind: *rng.pick(&["tree", "blob", "commit"]),
                        bytes: any(rng),
                    })
                    .collect(),
            });
            lines += &written_line_as_serde(&BlobPreview {
                oid: any(rng),
                bytes: any(rng),
                text: any(rng),
                truncated: any(rng),
                download_url: any(rng),
            });
            lines += &written_line_as_serde(&CommitSummary {
                oid: any(rng),
                parents: any(rng),
                author: any(rng),
                timestamp: any(rng),
                message: any(rng),
            });
        }
        check_recorded(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/json/browse-responses.txt"
            ),
            &lines,
        );
    }
}
