//! Endpoint handlers — pure logic over [`MultiRepo`] / [`Repository`],
//! returning owned bytes + status so [`router`](crate::router) doesn't
//! need to know anything about JSON.
//!
//! All payloads are hand-written JSON: the project does not pull
//! `serde_json` for a handful of stable shapes, and the surface is small
//! enough that escape logic stays local. Each handler is independently
//! unit-testable without booting a server.

use alt_git_codec::{ObjectId, ObjectKind, Tree};
use alt_repo::Repository;

use crate::{ApiError, MultiRepo};

/// `GET /api/version` — fixed compile-time identifiers for the build.
///
/// Stable shape: `{"schema_version":1, "version":"...", "build":"..."}`.
/// `version` mirrors `CARGO_PKG_VERSION`; `build` is a free-form tag the
/// deploy can override via `ALT_WEB_BUILD` at runtime, falling back to
/// the literal `"dev"`.
pub fn handle_version() -> (u16, Vec<u8>) {
    let version = env!("CARGO_PKG_VERSION");
    let build = std::env::var("ALT_WEB_BUILD").unwrap_or_else(|_| "dev".to_string());
    let body = format!(
        "{{\"schema_version\":1,\"version\":\"{}\",\"build\":{}}}",
        version,
        json_string(&build)
    );
    (200, body.into_bytes())
}

/// `GET /api/repos` — list every repo under the multi-repo root, with
/// the HEAD oid and current branch (best-effort) of each.
pub fn handle_repos(mr: &MultiRepo) -> Result<(u16, Vec<u8>), ApiError> {
    let names = mr.list()?;
    let mut items: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        let summary = match repo_summary(mr, &name) {
            Ok(s) => s,
            Err(e) => format!(
                "{{\"name\":{},\"error\":{}}}",
                json_string(&name),
                json_string(e.message())
            ),
        };
        items.push(summary);
    }
    let body = format!("{{\"schema_version\":1,\"repos\":[{}]}}", items.join(","));
    Ok((200, body.into_bytes()))
}

/// `GET /api/repos/{name}` — single repo summary (HEAD oid, ref count,
/// current branch).
pub fn handle_repo(mr: &MultiRepo, name: &str) -> Result<(u16, Vec<u8>), ApiError> {
    let body = repo_summary(mr, name)?;
    Ok((
        200,
        format!("{{\"schema_version\":1,\"repo\":{body}}}").into_bytes(),
    ))
}

fn repo_summary(mr: &MultiRepo, name: &str) -> Result<String, ApiError> {
    let repo = mr.open(name)?;
    let head = repo
        .rev_parse("HEAD")
        .map_err(|e| ApiError::Internal(format!("rev_parse HEAD: {e}")))?;
    let refs = repo
        .list_refs()
        .map_err(|e| ApiError::Internal(format!("list_refs: {e}")))?;
    let head_branch = current_branch(&repo).ok().flatten().unwrap_or_default();
    let head_str = head.map(|o| o.to_string()).unwrap_or_default();
    Ok(format!(
        "{{\"name\":{},\"head\":{},\"head_branch\":{},\"refs\":{}}}",
        json_string(name),
        json_string(&head_str),
        json_string(&head_branch),
        refs.len()
    ))
}

/// `GET /api/repos/{name}/refs` — every branch + tag, each with its
/// resolved commit oid.
pub fn handle_refs(mr: &MultiRepo, name: &str) -> Result<(u16, Vec<u8>), ApiError> {
    let repo = mr.open(name)?;
    let refs = repo
        .list_refs()
        .map_err(|e| ApiError::Internal(format!("list_refs: {e}")))?;
    let mut items: Vec<String> = Vec::with_capacity(refs.len());
    for (full_name, oid, target) in refs {
        let target_str = target.unwrap_or_default();
        items.push(format!(
            "{{\"name\":{},\"oid\":\"{}\",\"target\":{}}}",
            json_string(&full_name),
            oid,
            json_string(&target_str)
        ));
    }
    let body = format!("{{\"schema_version\":1,\"refs\":[{}]}}", items.join(","));
    Ok((200, body.into_bytes()))
}

/// `GET /api/repos/{name}/log?ref=<ref>&n=<n>&before=<oid>` — commit
/// list walked from `ref` (or HEAD), newest-first.
/// `before` skips until the named oid is seen (so the next page picks
/// up where the last one stopped).
pub fn handle_log(
    mr: &MultiRepo,
    name: &str,
    ref_name: Option<&str>,
    n: usize,
    before: Option<&str>,
) -> Result<(u16, Vec<u8>), ApiError> {
    const MAX: usize = 200;
    let n = n.clamp(1, MAX);
    let repo = mr.open(name)?;
    let start_oid = match ref_name {
        Some(r) => repo
            .rev_parse(r)
            .map_err(|e| ApiError::Internal(format!("rev_parse {r}: {e}")))?
            .ok_or_else(|| ApiError::NotFound(format!("ref {r}")))?,
        None => repo
            .rev_parse("HEAD")
            .map_err(|e| ApiError::Internal(format!("rev_parse HEAD: {e}")))?
            .ok_or_else(|| ApiError::NotFound("HEAD".to_string()))?,
    };
    let mut walker = repo
        .rev_walk(start_oid)
        .map_err(|e| ApiError::Internal(format!("rev_walk: {e}")))?;
    if let Some(b) = before {
        for item in walker.by_ref() {
            let (oid, _) = item.map_err(|e| ApiError::Internal(format!("walk: {e}")))?;
            if oid.to_string() == b {
                break;
            }
        }
    }
    let mut commits: Vec<String> = Vec::with_capacity(n);
    for item in walker.take(n) {
        let (oid, commit) = item.map_err(|e| ApiError::Internal(format!("walk: {e}")))?;
        commits.push(commit_summary(&oid, &commit));
    }
    let body = format!(
        "{{\"schema_version\":1,\"commits\":[{}]}}",
        commits.join(",")
    );
    Ok((200, body.into_bytes()))
}

/// `GET /api/repos/{name}/commits/{oid}` — full commit detail: oid,
/// tree, parents, author + committer (parsed from ident), message.
pub fn handle_commit(
    mr: &MultiRepo,
    name: &str,
    oid_str: &str,
) -> Result<(u16, Vec<u8>), ApiError> {
    let repo = mr.open(name)?;
    let oid = parse_oid(oid_str)?;
    let commit = repo
        .read_commit(&oid)
        .map_err(|e| ApiError::Internal(format!("read_commit {oid}: {e}")))?;
    let raw = String::from_utf8_lossy(commit.message().as_slice()).into_owned();
    let subject = subject_of(&raw);
    let body_part = match raw.find('\n') {
        Some(p) => raw[p + 1..].trim_start_matches('\n').to_string(),
        None => String::new(),
    };
    let tree = commit.tree().map(|o| o.to_string()).unwrap_or_default();
    let parents: Vec<String> = commit.parents().map(|p| format!("\"{p}\"")).collect();
    let (a_name, a_email, a_when) = author_parts(&commit);
    let (c_name, c_email, c_when) = committer_parts(&commit);
    let body = format!(
        "{{\"schema_version\":1,\"commit\":{{\
            \"oid\":\"{oid}\",\
            \"tree\":{},\
            \"parents\":[{}],\
            \"subject\":{},\
            \"body\":{},\
            \"author\":{{\"name\":{},\"email\":{},\"when\":{}}},\
            \"committer\":{{\"name\":{},\"email\":{},\"when\":{}}}\
        }}}}",
        json_string(&tree),
        parents.join(","),
        json_string(subject),
        json_string(&body_part),
        json_string(&a_name),
        json_string(&a_email),
        a_when,
        json_string(&c_name),
        json_string(&c_email),
        c_when,
    );
    Ok((200, body.into_bytes()))
}

/// `GET /api/repos/{name}/commits/{oid}/diff` — per-file diff between
/// the commit's tree and its first parent's tree. Each file picks the
/// richest available `kind`:
///
/// - `structured` — JSON / TOML, semantic key-level changes via
///   [`alt_diff::structured`].
/// - `part_aware` — PNG (chunk-level) or ZIP/OOXML (member-level)
///   summaries via [`alt_diff::part_aware`]. PNGs also carry the
///   perceptual fingerprint distance from [`alt_diff::perceptual`] for
///   "how visually different is this image".
/// - `text` — fall-through line diff (unified patch text).
/// - `binary` — neither side parseable; just `old_bytes` / `new_bytes`.
///
/// All kinds carry `old_oid` / `new_oid` (empty for add/delete sides) so
/// the frontend can request raw blobs to render images, etc.
pub fn handle_commit_diff(
    mr: &MultiRepo,
    name: &str,
    oid_str: &str,
) -> Result<(u16, Vec<u8>), ApiError> {
    let repo = mr.open(name)?;
    let oid = parse_oid(oid_str)?;
    let commit = repo
        .read_commit(&oid)
        .map_err(|e| ApiError::Internal(format!("read_commit {oid}: {e}")))?;
    let new_tree = commit
        .tree()
        .ok_or_else(|| ApiError::Internal(format!("commit {oid} has no tree")))?;
    let old_tree = commit.parents().next();

    let new_entries = flatten_tree(&repo, &new_tree)?;
    let old_entries = match &old_tree {
        Some(p) => {
            let parent_commit = repo
                .read_commit(p)
                .map_err(|e| ApiError::Internal(format!("read_commit {p}: {e}")))?;
            let parent_tree = parent_commit
                .tree()
                .ok_or_else(|| ApiError::Internal(format!("parent {p} has no tree")))?;
            flatten_tree(&repo, &parent_tree)?
        }
        None => Vec::new(),
    };
    let files = diff_trees(&repo, &old_entries, &new_entries)?;

    let mut entries: Vec<String> = Vec::with_capacity(files.len());
    for file in &files {
        entries.push(file.to_json());
    }
    let body = format!(
        "{{\"schema_version\":1,\"oid\":\"{oid}\",\"files\":[{}]}}",
        entries.join(",")
    );
    Ok((200, body.into_bytes()))
}

/// `GET /api/repos/{name}/blob/{oid}/raw` — raw blob bytes, with a
/// best-effort Content-Type based on a magic-byte sniff. Used by the
/// frontend to embed PNGs from the diff view as `<img>` elements.
pub fn handle_blob_raw(
    mr: &MultiRepo,
    name: &str,
    oid_str: &str,
) -> Result<(u16, Vec<u8>, &'static str), ApiError> {
    let repo = mr.open(name)?;
    let oid = parse_oid(oid_str)?;
    let raw = repo
        .read_object(&oid)
        .map_err(|e| ApiError::Internal(format!("read_object {oid}: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("blob {oid}")))?;
    if raw.kind != ObjectKind::Blob {
        return Err(ApiError::NotFound(format!(
            "object {oid} is {:?}, not a blob",
            raw.kind
        )));
    }
    let mime = sniff_mime(&raw.data);
    Ok((200, raw.data, mime))
}

fn sniff_mime(data: &[u8]) -> &'static str {
    if data.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return "image/png";
    }
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg";
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if data.starts_with(b"RIFF") && data.len() > 11 && &data[8..12] == b"WEBP" {
        return "image/webp";
    }
    if data.starts_with(b"PK\x03\x04") || data.starts_with(b"PK\x05\x06") {
        return "application/zip";
    }
    if alt_diff::is_binary(data) {
        "application/octet-stream"
    } else {
        "text/plain; charset=utf-8"
    }
}

/// `GET /api/repos/{name}/tree/{oid}` — list one tree level (mode, name,
/// oid, kind = "blob"/"tree"/"commit"). `oid` can be a commit ref-or-oid,
/// in which case the commit's root tree is listed.
pub fn handle_tree(mr: &MultiRepo, name: &str, spec: &str) -> Result<(u16, Vec<u8>), ApiError> {
    let repo = mr.open(name)?;
    let tree_oid = resolve_tree(&repo, spec)?;
    let tree = read_tree(&repo, &tree_oid)?;
    let mut items: Vec<String> = Vec::with_capacity(tree.entries.len());
    for entry in &tree.entries {
        let kind = match entry.mode.value() {
            0o040000 => "tree",
            0o160000 => "commit",
            _ => "blob",
        };
        items.push(format!(
            "{{\"mode\":\"{}\",\"name\":{},\"oid\":\"{}\",\"kind\":\"{}\"}}",
            entry.mode.as_str(),
            json_string(&String::from_utf8_lossy(entry.name.as_slice())),
            entry.oid,
            kind,
        ));
    }
    let body = format!(
        "{{\"schema_version\":1,\"oid\":\"{tree_oid}\",\"entries\":[{}]}}",
        items.join(",")
    );
    Ok((200, body.into_bytes()))
}

/// `GET /api/repos/{name}/file_history?path=<path>&ref=<ref>&n=<n>`
/// — every commit that changed `path`, newest-first, walking from
/// `ref` (or HEAD). Each entry carries the per-commit change kind
/// (added / changed / removed / renamed-into) plus the blob oids on
/// both sides so the frontend can deep-link to the diff or the blob.
///
/// Algorithm: walk commits; for each compare the blob oid at `path` in
/// the commit's tree vs the parent's tree. Different oids → changed
/// (or added if old=None, removed if new=None).
pub fn handle_file_history(
    mr: &MultiRepo,
    name: &str,
    ref_name: Option<&str>,
    path: &str,
    n: usize,
) -> Result<(u16, Vec<u8>), ApiError> {
    const MAX: usize = 200;
    let n = n.clamp(1, MAX);
    let repo = mr.open(name)?;
    if path.is_empty() {
        return Err(ApiError::NotFound("missing ?path=".to_string()));
    }
    let start_oid = match ref_name {
        Some(r) => repo
            .rev_parse(r)
            .map_err(|e| ApiError::Internal(format!("rev_parse {r}: {e}")))?
            .ok_or_else(|| ApiError::NotFound(format!("ref {r}")))?,
        None => repo
            .rev_parse("HEAD")
            .map_err(|e| ApiError::Internal(format!("rev_parse HEAD: {e}")))?
            .ok_or_else(|| ApiError::NotFound("HEAD".to_string()))?,
    };

    let walker = repo
        .rev_walk(start_oid)
        .map_err(|e| ApiError::Internal(format!("rev_walk: {e}")))?;

    let mut entries: Vec<String> = Vec::with_capacity(n);
    let mut last_oid: Option<ObjectId> = None;
    for item in walker {
        let (commit_oid, commit) = item.map_err(|e| ApiError::Internal(format!("walk: {e}")))?;

        let this = resolve_path_oid(&repo, &commit, path)?;
        // Parent state: if no parent, treat as None (this commit
        // introduced everything). For a merge we look at the first
        // parent only, matching `alt log -p`'s behaviour.
        let parent_oid_opt = commit.parents().next();
        let parent = match parent_oid_opt {
            Some(p) => {
                let parent_commit = repo
                    .read_commit(&p)
                    .map_err(|e| ApiError::Internal(format!("read_commit {p}: {e}")))?;
                resolve_path_oid(&repo, &parent_commit, path)?
            }
            None => None,
        };

        let change = match (parent, this) {
            (None, None) => {
                last_oid = None;
                continue;
            }
            (Some(p), Some(t)) if p == t => {
                last_oid = Some(t);
                continue;
            }
            (None, Some(t)) => {
                let entry = file_history_entry(&commit_oid, &commit, "added", None, Some(&t));
                last_oid = Some(t);
                entry
            }
            (Some(p), None) => {
                let entry = file_history_entry(&commit_oid, &commit, "removed", Some(&p), None);
                last_oid = None;
                entry
            }
            (Some(p), Some(t)) => {
                let entry = file_history_entry(&commit_oid, &commit, "changed", Some(&p), Some(&t));
                last_oid = Some(t);
                entry
            }
        };
        entries.push(change);
        if entries.len() >= n {
            break;
        }
    }
    let _ = last_oid;

    let body = format!(
        "{{\"schema_version\":1,\"path\":{},\"commits\":[{}]}}",
        json_string(path),
        entries.join(",")
    );
    Ok((200, body.into_bytes()))
}

fn resolve_path_oid(
    repo: &Repository,
    commit: &alt_git_codec::Commit,
    path: &str,
) -> Result<Option<ObjectId>, ApiError> {
    let Some(tree_oid) = commit.tree() else {
        return Ok(None);
    };
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut current = tree_oid;
    for (i, seg) in segments.iter().enumerate() {
        let tree = read_tree(repo, &current)?;
        let Some(entry) = tree
            .entries
            .iter()
            .find(|e| String::from_utf8_lossy(e.name.as_slice()) == **seg)
        else {
            return Ok(None);
        };
        // last segment is the blob/tree we want
        if i == segments.len() - 1 {
            return Ok(Some(entry.oid));
        }
        // non-leaf must be a subtree to continue
        if entry.mode.value() != 0o040000 {
            return Ok(None);
        }
        current = entry.oid;
    }
    Ok(Some(current))
}

fn file_history_entry(
    commit_oid: &ObjectId,
    commit: &alt_git_codec::Commit,
    change: &str,
    old_oid: Option<&ObjectId>,
    new_oid: Option<&ObjectId>,
) -> String {
    let raw = String::from_utf8_lossy(commit.message().as_slice()).into_owned();
    let subject = subject_of(&raw);
    let (a_name, a_email, a_when) = author_parts(commit);
    let old_str = old_oid.map(|o| o.to_string()).unwrap_or_default();
    let new_str = new_oid.map(|o| o.to_string()).unwrap_or_default();
    format!(
        "{{\"oid\":\"{commit_oid}\",\"subject\":{},\"author\":{{\"name\":{},\"email\":{},\"when\":{}}},\
         \"change\":\"{change}\",\"old_oid\":{},\"new_oid\":{}}}",
        json_string(subject),
        json_string(&a_name),
        json_string(&a_email),
        a_when,
        json_string(&old_str),
        json_string(&new_str),
    )
}

/// `GET /api/repos/{name}/blob/{oid}` — blob content with size + binary
/// flag. Text blobs return UTF-8 (lossy); binary blobs return only the
/// size, leaving the content out so the JSON stays small.
pub fn handle_blob(mr: &MultiRepo, name: &str, oid_str: &str) -> Result<(u16, Vec<u8>), ApiError> {
    let repo = mr.open(name)?;
    let oid = parse_oid(oid_str)?;
    let raw = repo
        .read_object(&oid)
        .map_err(|e| ApiError::Internal(format!("read_object {oid}: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("blob {oid}")))?;
    if raw.kind != ObjectKind::Blob {
        return Err(ApiError::NotFound(format!(
            "object {oid} is {:?}, not a blob",
            raw.kind
        )));
    }
    let size = raw.data.len();
    let binary = alt_diff::is_binary(&raw.data);
    let body = if binary {
        format!(
            "{{\"schema_version\":1,\"oid\":\"{oid}\",\"size\":{size},\"binary\":true,\"content\":null}}"
        )
    } else {
        let text = String::from_utf8_lossy(&raw.data).into_owned();
        format!(
            "{{\"schema_version\":1,\"oid\":\"{oid}\",\"size\":{size},\"binary\":false,\"content\":{}}}",
            json_string(&text)
        )
    };
    Ok((200, body.into_bytes()))
}

fn parse_oid(s: &str) -> Result<ObjectId, ApiError> {
    ObjectId::from_hex(s.as_bytes())
        .map_err(|_| ApiError::NotFound(format!("invalid object id: {s}")))
}

fn resolve_tree(repo: &Repository, spec: &str) -> Result<ObjectId, ApiError> {
    // Try parsing as a tree oid directly; if it's not a tree, fall back
    // to resolving as a commit / ref → root tree.
    if let Ok(oid) = ObjectId::from_hex(spec.as_bytes())
        && let Ok(Some(obj)) = repo.read_object(&oid)
    {
        match obj.kind {
            ObjectKind::Tree => return Ok(oid),
            ObjectKind::Commit => {
                let commit = repo
                    .read_commit(&oid)
                    .map_err(|e| ApiError::Internal(format!("read_commit {oid}: {e}")))?;
                return commit
                    .tree()
                    .ok_or_else(|| ApiError::Internal(format!("commit {oid} has no tree")));
            }
            _ => {}
        }
    }
    let resolved = repo
        .rev_parse(spec)
        .map_err(|e| ApiError::Internal(format!("rev_parse {spec}: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("ref {spec}")))?;
    let commit = repo
        .read_commit(&resolved)
        .map_err(|e| ApiError::Internal(format!("read_commit {resolved}: {e}")))?;
    commit
        .tree()
        .ok_or_else(|| ApiError::Internal(format!("commit {resolved} has no tree")))
}

fn read_tree(repo: &Repository, oid: &ObjectId) -> Result<Tree, ApiError> {
    let raw = repo
        .read_object(oid)
        .map_err(|e| ApiError::Internal(format!("read_object {oid}: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("tree {oid}")))?;
    if raw.kind != ObjectKind::Tree {
        return Err(ApiError::NotFound(format!(
            "object {oid} is {:?}, not a tree",
            raw.kind
        )));
    }
    Tree::parse(&raw.data, repo.algo()).map_err(|e| ApiError::Internal(format!("tree {oid}: {e}")))
}

/// Flatten a tree to `(path, blob_oid)` pairs by walking subtrees,
/// joining names with `/`. Used by [`diff_trees`].
fn flatten_tree(
    repo: &Repository,
    tree_oid: &ObjectId,
) -> Result<Vec<(String, ObjectId)>, ApiError> {
    let mut out = Vec::new();
    walk(repo, tree_oid, "", &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn walk(
    repo: &Repository,
    tree_oid: &ObjectId,
    prefix: &str,
    out: &mut Vec<(String, ObjectId)>,
) -> Result<(), ApiError> {
    let tree = read_tree(repo, tree_oid)?;
    for entry in tree.entries {
        let name = String::from_utf8_lossy(entry.name.as_slice()).into_owned();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        match entry.mode.value() {
            0o040000 => walk(repo, &entry.oid, &path, out)?,
            0o160000 => {} // submodule — skip in diff for now
            _ => out.push((path, entry.oid)),
        }
    }
    Ok(())
}

/// One ZIP/OOXML member, after the part-aware summary has classified it.
/// Carries an optional inner unified-diff patch so the SPA can expand a
/// `changed` text member (e.g. `word/document.xml`) into a real per-line
/// diff instead of stopping at "changed, 726 → 845 bytes".
pub(crate) struct PartRow {
    pub name: String,
    pub change: alt_diff::part_aware::PartChange,
    pub text_patch: Option<String>,
}

/// One file's diff in the response. The renderer picks shape by `kind`.
pub(crate) enum FileDiff {
    Text {
        path: String,
        old_oid: String,
        new_oid: String,
        patch: String,
    },
    Structured {
        path: String,
        format: &'static str,
        old_oid: String,
        new_oid: String,
        paths: Vec<(String, alt_diff::structured::PathChange)>,
    },
    PartAware {
        path: String,
        format: &'static str,
        old_oid: String,
        new_oid: String,
        old_bytes: usize,
        new_bytes: usize,
        /// One row per stable part name; `change` is the part-aware
        /// classification, `text_patch` is a unified diff of the
        /// member's *inflated* body when both sides are text (only
        /// emitted for `Changed` parts that survived the binary check
        /// + fit within the size cap).
        parts: Vec<PartRow>,
        /// Perceptual fingerprint distance for PNG (0..=1). `None` when
        /// the kind isn't PNG or fingerprint couldn't be computed.
        perceptual_distance: Option<f64>,
    },
    Binary {
        path: String,
        old_oid: String,
        new_oid: String,
        old_bytes: usize,
        new_bytes: usize,
    },
}

impl FileDiff {
    fn to_json(&self) -> String {
        match self {
            FileDiff::Text {
                path,
                old_oid,
                new_oid,
                patch,
            } => format!(
                "{{\"kind\":\"text\",\"path\":{},\"old_oid\":{},\"new_oid\":{},\"patch\":{}}}",
                json_string(path),
                json_string(old_oid),
                json_string(new_oid),
                json_string(patch),
            ),
            FileDiff::Structured {
                path,
                format,
                old_oid,
                new_oid,
                paths,
            } => {
                let items: Vec<String> = paths
                    .iter()
                    .map(|(p, change)| match change {
                        alt_diff::structured::PathChange::Changed { old_repr, new_repr } => {
                            format!(
                                "{{\"path\":{},\"change\":\"changed\",\"old\":{},\"new\":{}}}",
                                json_string(p),
                                json_string(old_repr),
                                json_string(new_repr),
                            )
                        }
                        alt_diff::structured::PathChange::Added { new_repr } => format!(
                            "{{\"path\":{},\"change\":\"added\",\"new\":{}}}",
                            json_string(p),
                            json_string(new_repr),
                        ),
                        alt_diff::structured::PathChange::Removed { old_repr } => format!(
                            "{{\"path\":{},\"change\":\"removed\",\"old\":{}}}",
                            json_string(p),
                            json_string(old_repr),
                        ),
                    })
                    .collect();
                format!(
                    "{{\"kind\":\"structured\",\"path\":{},\"format\":\"{}\",\"old_oid\":{},\"new_oid\":{},\"paths\":[{}]}}",
                    json_string(path),
                    format,
                    json_string(old_oid),
                    json_string(new_oid),
                    items.join(","),
                )
            }
            FileDiff::PartAware {
                path,
                format,
                old_oid,
                new_oid,
                old_bytes,
                new_bytes,
                parts,
                perceptual_distance,
            } => {
                let items: Vec<String> = parts
                    .iter()
                    .map(|row| {
                        let head = match &row.change {
                            alt_diff::part_aware::PartChange::Same => format!(
                                "{{\"name\":{},\"change\":\"same\"",
                                json_string(&row.name)
                            ),
                            alt_diff::part_aware::PartChange::Changed {
                                old_bytes: ob,
                                new_bytes: nb,
                            } => format!(
                                "{{\"name\":{},\"change\":\"changed\",\"old_bytes\":{},\"new_bytes\":{}",
                                json_string(&row.name),
                                ob,
                                nb,
                            ),
                            alt_diff::part_aware::PartChange::Added { new_bytes: nb } => format!(
                                "{{\"name\":{},\"change\":\"added\",\"new_bytes\":{}",
                                json_string(&row.name),
                                nb,
                            ),
                            alt_diff::part_aware::PartChange::Removed { old_bytes: ob } => format!(
                                "{{\"name\":{},\"change\":\"removed\",\"old_bytes\":{}",
                                json_string(&row.name),
                                ob,
                            ),
                        };
                        match &row.text_patch {
                            Some(patch) => format!(
                                "{head},\"text_patch\":{}}}",
                                json_string(patch)
                            ),
                            None => format!("{head}}}"),
                        }
                    })
                    .collect();
                let pd = match perceptual_distance {
                    Some(d) => format!("{d}"),
                    None => "null".to_string(),
                };
                format!(
                    "{{\"kind\":\"part_aware\",\"path\":{},\"format\":\"{}\",\"old_oid\":{},\"new_oid\":{},\"old_bytes\":{},\"new_bytes\":{},\"perceptual_distance\":{},\"parts\":[{}]}}",
                    json_string(path),
                    format,
                    json_string(old_oid),
                    json_string(new_oid),
                    old_bytes,
                    new_bytes,
                    pd,
                    items.join(","),
                )
            }
            FileDiff::Binary {
                path,
                old_oid,
                new_oid,
                old_bytes,
                new_bytes,
            } => format!(
                "{{\"kind\":\"binary\",\"path\":{},\"old_oid\":{},\"new_oid\":{},\"old_bytes\":{},\"new_bytes\":{}}}",
                json_string(path),
                json_string(old_oid),
                json_string(new_oid),
                old_bytes,
                new_bytes,
            ),
        }
    }
}

fn diff_trees(
    repo: &Repository,
    old: &[(String, ObjectId)],
    new: &[(String, ObjectId)],
) -> Result<Vec<FileDiff>, ApiError> {
    use std::collections::BTreeMap;
    let old_map: BTreeMap<&str, &ObjectId> = old.iter().map(|(p, o)| (p.as_str(), o)).collect();
    let new_map: BTreeMap<&str, &ObjectId> = new.iter().map(|(p, o)| (p.as_str(), o)).collect();
    let mut all: Vec<&str> = old_map.keys().chain(new_map.keys()).copied().collect();
    all.sort();
    all.dedup();

    let mut out = Vec::new();
    for path in all {
        let o_oid = old_map.get(path);
        let n_oid = new_map.get(path);
        if let (Some(a), Some(b)) = (o_oid, n_oid)
            && a == b
        {
            continue;
        }
        let old_bytes = match o_oid {
            Some(o) => read_blob(repo, o)?,
            None => Vec::new(),
        };
        let new_bytes = match n_oid {
            Some(o) => read_blob(repo, o)?,
            None => Vec::new(),
        };
        let old_oid_str = o_oid.map(|o| o.to_string()).unwrap_or_default();
        let new_oid_str = n_oid.map(|o| o.to_string()).unwrap_or_default();

        // Pick the richest diff kind we can produce. Order: structured
        // (semantic JSON/TOML) → part-aware (PNG chunk / ZIP / OOXML) →
        // text unified → opaque binary.
        if let Some(summary) = alt_diff::structured::summary_for_path(path, &old_bytes, &new_bytes)
        {
            let format = match summary.kind {
                alt_diff::structured::StructKind::Json => "json",
                alt_diff::structured::StructKind::Toml => "toml",
            };
            out.push(FileDiff::Structured {
                path: path.to_string(),
                format,
                old_oid: old_oid_str,
                new_oid: new_oid_str,
                paths: summary.paths,
            });
            continue;
        }
        if let Some(summary) = alt_diff::part_aware::summary(&old_bytes, &new_bytes) {
            let format = match summary.kind {
                alt_diff::part_aware::PartKind::Png => "png",
                alt_diff::part_aware::PartKind::Zip => "zip",
            };
            let perceptual_distance = if matches!(summary.kind, alt_diff::part_aware::PartKind::Png)
            {
                let old_fp = alt_diff::perceptual::fingerprint(&old_bytes);
                let new_fp = alt_diff::perceptual::fingerprint(&new_bytes);
                alt_diff::perceptual::distance(old_fp, new_fp)
            } else {
                None
            };

            // For ZIP / OOXML, decode every member's inflated body on
            // both sides, then attach a unified-diff patch to each
            // `changed` member whose bodies are both text-shaped. This
            // is where alt's binary-aware diff stops being just
            // "something changed in word/document.xml" and becomes
            // "here are the lines that changed in word/document.xml".
            let parts = enrich_parts_with_text_patches(
                &summary.kind,
                summary.parts,
                &old_bytes,
                &new_bytes,
            );

            out.push(FileDiff::PartAware {
                path: path.to_string(),
                format,
                old_oid: old_oid_str,
                new_oid: new_oid_str,
                old_bytes: old_bytes.len(),
                new_bytes: new_bytes.len(),
                parts,
                perceptual_distance,
            });
            continue;
        }
        if alt_diff::is_binary(&old_bytes) || alt_diff::is_binary(&new_bytes) {
            out.push(FileDiff::Binary {
                path: path.to_string(),
                old_oid: old_oid_str,
                new_oid: new_oid_str,
                old_bytes: old_bytes.len(),
                new_bytes: new_bytes.len(),
            });
            continue;
        }
        let mut patch = Vec::new();
        write_unified_with_headers(&mut patch, path, &old_bytes, &new_bytes);
        out.push(FileDiff::Text {
            path: path.to_string(),
            old_oid: old_oid_str,
            new_oid: new_oid_str,
            patch: String::from_utf8_lossy(&patch).into_owned(),
        });
    }
    Ok(out)
}

/// Cap on a single ZIP member's inflated size for inner-diff purposes.
/// Above this we drop `text_patch` and let the part-aware summary stand
/// alone — protects the JSON response from blowing up on a 50 MB XML
/// inside an OOXML.
const MAX_PART_BODY: usize = 1 << 20;

/// Cap on the generated unified diff text per file. A single text patch
/// is usually a few KB; anything past this is almost certainly a tree
/// rewrite that's better explored member-by-member than inlined.
const MAX_PATCH: usize = 128 * 1024;

fn enrich_parts_with_text_patches(
    kind: &alt_diff::part_aware::PartKind,
    parts: Vec<(String, alt_diff::part_aware::PartChange)>,
    old_bytes: &[u8],
    new_bytes: &[u8],
) -> Vec<PartRow> {
    // Only the ZIP family carries member bodies worth inflating. PNGs
    // are dense byte streams — no per-chunk text patch makes sense.
    let bodies = match kind {
        alt_diff::part_aware::PartKind::Zip => {
            let old = alt_diff::part_aware::zip_member_bodies(old_bytes, MAX_PART_BODY);
            let new = alt_diff::part_aware::zip_member_bodies(new_bytes, MAX_PART_BODY);
            Some((old, new))
        }
        alt_diff::part_aware::PartKind::Png => None,
    };

    parts
        .into_iter()
        .map(|(name, change)| {
            let text_patch = match (&change, &bodies) {
                (
                    alt_diff::part_aware::PartChange::Changed { .. },
                    Some((Some(old_map), Some(new_map))),
                ) => {
                    let old_body = old_map.get(&name).and_then(|b| b.as_ref());
                    let new_body = new_map.get(&name).and_then(|b| b.as_ref());
                    match (old_body, new_body) {
                        (Some(o), Some(n))
                            if !alt_diff::is_binary(o) && !alt_diff::is_binary(n) =>
                        {
                            let mut buf = Vec::new();
                            let _ = std::io::Write::write_fmt(
                                &mut buf,
                                format_args!("--- a/{name}\n+++ b/{name}\n"),
                            );
                            alt_diff::write_unified(&mut buf, o, n, 3);
                            if buf.len() > MAX_PATCH {
                                None
                            } else {
                                Some(String::from_utf8_lossy(&buf).into_owned())
                            }
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            PartRow {
                name,
                change,
                text_patch,
            }
        })
        .collect()
}

fn read_blob(repo: &Repository, oid: &ObjectId) -> Result<Vec<u8>, ApiError> {
    let raw = repo
        .read_object(oid)
        .map_err(|e| ApiError::Internal(format!("read_object {oid}: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("blob {oid}")))?;
    if raw.kind != ObjectKind::Blob {
        return Err(ApiError::Internal(format!(
            "object {oid} expected blob, got {:?}",
            raw.kind
        )));
    }
    Ok(raw.data)
}

fn write_unified_with_headers(out: &mut Vec<u8>, path: &str, old: &[u8], new: &[u8]) {
    use std::io::Write;
    let _ = writeln!(out, "--- a/{path}");
    let _ = writeln!(out, "+++ b/{path}");
    if alt_diff::is_binary(old) || alt_diff::is_binary(new) {
        let _ = writeln!(out, "Binary files differ");
        return;
    }
    alt_diff::write_unified(out, old, new, 3);
}

fn committer_parts(commit: &alt_git_codec::Commit) -> (String, String, i64) {
    let Some(ident) = commit.committer() else {
        return (String::new(), String::new(), 0);
    };
    parse_ident(&String::from_utf8_lossy(ident))
}

fn parse_ident(s: &str) -> (String, String, i64) {
    let (name, rest) = match s.find('<') {
        Some(p) => (s[..p].trim_end().to_string(), &s[p + 1..]),
        None => return (s.to_string(), String::new(), 0),
    };
    let (email, after) = match rest.find('>') {
        Some(p) => (rest[..p].to_string(), rest[p + 1..].trim()),
        None => (rest.to_string(), ""),
    };
    let when = after
        .split_whitespace()
        .next()
        .and_then(|t| t.parse::<i64>().ok())
        .unwrap_or(0);
    (name, email, when)
}

fn current_branch(repo: &Repository) -> Result<Option<String>, ApiError> {
    let refs = repo
        .list_refs()
        .map_err(|e| ApiError::Internal(format!("list_refs: {e}")))?;
    let head = repo
        .rev_parse("HEAD")
        .map_err(|e| ApiError::Internal(format!("rev_parse HEAD: {e}")))?;
    let Some(head) = head else {
        return Ok(None);
    };
    for (name, oid, _) in &refs {
        if oid == &head && name.starts_with("refs/heads/") {
            return Ok(Some(name.trim_start_matches("refs/heads/").to_string()));
        }
    }
    Ok(None)
}

fn commit_summary(oid: &alt_git_codec::ObjectId, commit: &alt_git_codec::Commit) -> String {
    let raw = String::from_utf8_lossy(commit.message().as_slice()).into_owned();
    let subject = subject_of(&raw);
    let (name, email, when) = author_parts(commit);
    format!(
        "{{\"oid\":\"{}\",\"subject\":{},\"author\":{{\"name\":{},\"email\":{},\"when\":{}}}}}",
        oid,
        json_string(subject),
        json_string(&name),
        json_string(&email),
        when,
    )
}

/// Parse a git ident line (`Name <email> 1234567890 +0900`) into
/// `(name, email, epoch_seconds)`. Missing fields fall back to empty
/// string / 0 so a malformed commit doesn't trip the response.
fn author_parts(commit: &alt_git_codec::Commit) -> (String, String, i64) {
    let Some(ident) = commit.author() else {
        return (String::new(), String::new(), 0);
    };
    parse_ident(&String::from_utf8_lossy(ident))
}

/// First line of a commit message; subjects are the only line a landing
/// page needs to render.
fn subject_of(message: &str) -> &str {
    match message.find('\n') {
        Some(p) => &message[..p],
        None => message,
    }
}

/// Hand-rolled JSON string encoder — the surface is small and stable, so
/// avoiding `serde_json` keeps the dep tree to `tiny_http + alt_repo`.
/// Escapes `"`, `\`, control characters; everything else (including
/// non-ASCII) is passed through, since the project's commit subjects are
/// UTF-8 by contract.
pub(crate) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_string_escapes_quote_and_backslash() {
        assert_eq!(json_string(r#"hi"there"#), r#""hi\"there""#);
        assert_eq!(json_string("a\\b"), r#""a\\b""#);
        assert_eq!(json_string("first\nsecond"), r#""first\nsecond""#);
        assert_eq!(json_string("tab\there"), r#""tab\there""#);
    }

    #[test]
    fn json_string_passes_unicode_through() {
        let s = json_string("こんにちは alt");
        assert!(s.contains("こんにちは alt"), "got {s}");
    }

    #[test]
    fn subject_takes_first_line() {
        assert_eq!(subject_of("hello\nbody\nrest"), "hello");
        assert_eq!(subject_of("oneliner"), "oneliner");
        assert_eq!(subject_of(""), "");
    }

    #[test]
    fn handle_version_emits_stable_shape() {
        let (status, body) = handle_version();
        assert_eq!(status, 200);
        let s = String::from_utf8(body).unwrap();
        assert!(s.contains("\"schema_version\":1"), "got {s}");
        assert!(s.contains("\"version\":\"0.0.0\""), "got {s}");
        assert!(s.contains("\"build\":"), "got {s}");
    }
}
