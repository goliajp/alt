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

/// `GET /api/repos/{name}/commits/{oid}/footprint` — per-commit
/// "how much did alt grow because of this commit." Walks every file in
/// the commit's tree vs its first parent's tree, and for each path
/// whose blob oid changed computes the set of leaf chunks on each
/// side. Bytes split into:
///
/// - **net new** — chunks present in the new blob's storage view that
///   the old blob's storage view didn't share. This is what alt
///   actually had to write to disk for this commit, file by file.
/// - **shared with parent** — chunks present in both. Bytes alt got
///   for free thanks to CDC + prism dedup.
///
/// First-parent only (matches commit diff). For a root commit (no
/// parent) everything counts as net new.
pub fn handle_commit_footprint(
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
    let parent_oid = commit.parents().next();
    let old_files: Vec<(String, ObjectId)> = match parent_oid {
        Some(p) => {
            let parent = repo
                .read_commit(&p)
                .map_err(|e| ApiError::Internal(format!("read_commit parent {p}: {e}")))?;
            match parent.tree() {
                Some(t) => flatten_tree(&repo, &t)?,
                None => Vec::new(),
            }
        }
        None => Vec::new(),
    };
    let new_files = flatten_tree(&repo, &new_tree)?;

    use std::collections::BTreeMap;
    let old_map: BTreeMap<&str, &ObjectId> =
        old_files.iter().map(|(p, o)| (p.as_str(), o)).collect();
    let new_map: BTreeMap<&str, &ObjectId> =
        new_files.iter().map(|(p, o)| (p.as_str(), o)).collect();

    let mut paths: Vec<&str> = old_map.keys().chain(new_map.keys()).copied().collect();
    paths.sort();
    paths.dedup();

    let mut file_jsons: Vec<String> = Vec::new();
    let mut tot_net_new_chunks = 0u64;
    let mut tot_shared_chunks = 0u64;
    let mut tot_net_new_bytes = 0u64;
    let mut tot_shared_bytes = 0u64;

    for path in paths {
        let old_o = old_map.get(path).copied();
        let new_o = new_map.get(path).copied();
        if old_o == new_o {
            // identical blob — file didn't move, no footprint to report
            continue;
        }

        let (old_chunks, _old_logical) = chunks_for(&repo, old_o)?;
        let (new_chunks, _new_logical) = chunks_for(&repo, new_o)?;

        use std::collections::HashMap;
        let old_set: HashMap<_, _> = old_chunks
            .iter()
            .map(|c| (c.chunk_id, c.stored_len as u64))
            .collect();
        let mut net_new = 0u64;
        let mut net_new_bytes = 0u64;
        let mut shared = 0u64;
        let mut shared_bytes = 0u64;
        for c in &new_chunks {
            if old_set.contains_key(&c.chunk_id) {
                shared += 1;
                shared_bytes += c.stored_len as u64;
            } else {
                net_new += 1;
                net_new_bytes += c.stored_len as u64;
            }
        }

        let kind = match (old_o, new_o) {
            (None, Some(_)) => "added",
            (Some(_), None) => "removed",
            _ => "changed",
        };

        tot_net_new_chunks += net_new;
        tot_shared_chunks += shared;
        tot_net_new_bytes += net_new_bytes;
        tot_shared_bytes += shared_bytes;

        file_jsons.push(format!(
            "{{\"path\":{},\"change\":\"{kind}\",\"old_blob\":{},\"new_blob\":{},\
             \"old_chunks\":{},\"new_chunks\":{},\
             \"net_new_chunks\":{},\"shared_chunks\":{},\
             \"net_new_bytes\":{},\"shared_bytes\":{}}}",
            json_string(path),
            json_string(&old_o.map(|o| o.to_string()).unwrap_or_default()),
            json_string(&new_o.map(|o| o.to_string()).unwrap_or_default()),
            old_chunks.len(),
            new_chunks.len(),
            net_new,
            shared,
            net_new_bytes,
            shared_bytes,
        ));
    }

    let parent_str = parent_oid.map(|p| p.to_string()).unwrap_or_default();
    let body = format!(
        "{{\"schema_version\":1,\"oid\":\"{oid}\",\"parent\":{},\
         \"totals\":{{\
            \"net_new_chunks\":{},\"shared_chunks\":{},\
            \"net_new_bytes\":{},\"shared_bytes\":{}\
         }},\
         \"files\":[{}]}}",
        json_string(&parent_str),
        tot_net_new_chunks,
        tot_shared_chunks,
        tot_net_new_bytes,
        tot_shared_bytes,
        file_jsons.join(","),
    );
    Ok((200, body.into_bytes()))
}

fn chunks_for(
    repo: &Repository,
    oid: Option<&ObjectId>,
) -> Result<(Vec<alt_odb::ChunkInfo>, u64), ApiError> {
    let Some(o) = oid else {
        return Ok((Vec::new(), 0));
    };
    let view = repo
        .storage_view(o)
        .map_err(|e| ApiError::Internal(format!("storage_view {o}: {e}")))?;
    let Some(view) = view else {
        return Ok((Vec::new(), 0));
    };
    Ok((view.chunks.chunks, view.logical_size))
}

/// `GET /api/repos/{name}/storage_stats` — repo-wide aggregate
/// storage report: total logical bytes alt is responsible for, total
/// on-disk bytes, tier 0 vs tier 1 blob split, chunk encoding
/// distribution (raw / zstd / delta), and per-prism hit counts. This
/// surfaces the "alt is X% the size of logical content" headline
/// number for the repo home page.
pub fn handle_storage_stats(mr: &MultiRepo, name: &str) -> Result<(u16, Vec<u8>), ApiError> {
    let repo = mr.open(name)?;
    let stats = repo
        .storage_stats()
        .map_err(|e| ApiError::Internal(format!("storage_stats: {e}")))?
        .ok_or_else(|| {
            ApiError::NotFound("repo is git-backed; storage stats are alt-only".to_string())
        })?;

    let alt_dir = mr.root().join(name).join(".alt");
    let disk_bytes = dir_size(&alt_dir);

    let prisms_json: Vec<String> = stats
        .prisms
        .iter()
        .map(|(prism_id, ps)| {
            let label = match prism_id {
                1 => "deflate",
                2 => "zip",
                3 => "png",
                _ => "other",
            };
            format!(
                "{{\"id\":{},\"label\":\"{}\",\"blobs\":{},\"parts\":{}}}",
                prism_id, label, ps.blobs, ps.parts
            )
        })
        .collect();

    let body = format!(
        "{{\"schema_version\":1,\
         \"objects\":{{\"total\":{},\"blobs\":{},\"trees\":{},\"commits\":{},\"tags\":{}}},\
         \"logical_total\":{},\
         \"stored_total\":{},\
         \"disk_total\":{},\
         \"chunks\":{{\"total\":{},\"logical_total\":{}}},\
         \"tier\":{{\"verbatim\":{},\"prismatic\":{}}},\
         \"encoding\":{{\
            \"raw\":{{\"chunks\":{},\"stored\":{}}},\
            \"zstd\":{{\"chunks\":{},\"stored\":{}}},\
            \"delta\":{{\"chunks\":{},\"stored\":{}}}\
         }},\
         \"prisms\":[{}]\
        }}",
        stats.object_count,
        stats.blobs,
        stats.trees,
        stats.commits,
        stats.tags,
        stats.logical_total,
        stats.stored_total,
        disk_bytes,
        stats.chunks_total,
        stats.chunk_logical_total,
        stats.tier0_count,
        stats.tier1_count,
        stats.raw_chunks,
        stats.raw_stored,
        stats.zstd_chunks,
        stats.zstd_stored,
        stats.delta_chunks,
        stats.delta_stored,
        prisms_json.join(","),
    );
    Ok((200, body.into_bytes()))
}

fn dir_size(p: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(p) else {
        return 0;
    };
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            total = total.saturating_add(dir_size(&e.path()));
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// `GET /api/repos/{name}/storage/{oid}` — alt's physical storage
/// layout for `oid`: tier (0 = verbatim CDC, 1 = prism-decomposed),
/// prism id + parts when Tier 1, and one record per leaf chunk
/// (encoding Raw/Zstd/Delta, orig vs stored bytes). This is the
/// reader-facing answer to "what does alt actually store as the
/// delta" — git would have no equivalent.
pub fn handle_storage(
    mr: &MultiRepo,
    name: &str,
    oid_str: &str,
) -> Result<(u16, Vec<u8>), ApiError> {
    let repo = mr.open(name)?;
    let oid = parse_oid(oid_str)?;
    let view = repo
        .storage_view(&oid)
        .map_err(|e| ApiError::Internal(format!("storage_view {oid}: {e}")))?
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "no storage layout for {oid} (object unknown, or repo is git-backed)"
            ))
        })?;

    let chunks_json: Vec<String> = view
        .chunks
        .chunks
        .iter()
        .map(|c| {
            let enc = match c.encoding {
                alt_store::Encoding::Raw => "raw",
                alt_store::Encoding::Zstd => "zstd",
                alt_store::Encoding::Delta => "delta",
            };
            format!(
                "{{\"chunk_id\":{},\"encoding\":\"{}\",\"orig_len\":{},\"stored_len\":{}}}",
                json_string(&hex32(&c.chunk_id.0)),
                enc,
                c.orig_len,
                c.stored_len,
            )
        })
        .collect();

    let tier1_json = match &view.tier1 {
        Some(t1) => {
            let parts: Vec<String> = t1.parts.iter().map(|p| json_string(&hex32(&p.0))).collect();
            format!(
                "{{\"prism\":{},\"recipe_len\":{},\"record_blob\":{},\"parts\":[{}]}}",
                t1.prism.0,
                t1.recipe_len,
                json_string(&hex32(&t1.record_blob.0)),
                parts.join(","),
            )
        }
        None => "null".to_string(),
    };

    let kind = match view.kind {
        alt_git_codec::ObjectKind::Blob => "blob",
        alt_git_codec::ObjectKind::Tree => "tree",
        alt_git_codec::ObjectKind::Commit => "commit",
        alt_git_codec::ObjectKind::Tag => "tag",
    };

    let body = format!(
        "{{\"schema_version\":1,\
         \"git_oid\":\"{}\",\
         \"blob_id\":{},\
         \"kind\":\"{}\",\
         \"logical_size\":{},\
         \"tier\":{},\
         \"tier1\":{},\
         \"chunks\":{{\
             \"leaf_count\":{},\
             \"logical_total\":{},\
             \"stored_total\":{},\
             \"entries\":[{}]\
         }}\
        }}",
        view.git_oid,
        json_string(&hex32(&view.blob_id.0)),
        kind,
        view.logical_size,
        if view.tier1.is_some() { 1 } else { 0 },
        tier1_json,
        view.chunks.leaf_count,
        view.chunks.logical_total,
        view.chunks.stored_total,
        chunks_json.join(","),
    );
    Ok((200, body.into_bytes()))
}

fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        s.push_str(&format!("{:02x}", byte));
    }
    s
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
                let entry =
                    file_history_entry(&repo, &commit_oid, &commit, "added", None, Some(&t), path)?;
                last_oid = Some(t);
                entry
            }
            (Some(p), None) => {
                let entry = file_history_entry(
                    &repo,
                    &commit_oid,
                    &commit,
                    "removed",
                    Some(&p),
                    None,
                    path,
                )?;
                last_oid = None;
                entry
            }
            (Some(p), Some(t)) => {
                let entry = file_history_entry(
                    &repo,
                    &commit_oid,
                    &commit,
                    "changed",
                    Some(&p),
                    Some(&t),
                    path,
                )?;
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
    repo: &Repository,
    commit_oid: &ObjectId,
    commit: &alt_git_codec::Commit,
    change: &str,
    old_oid: Option<&ObjectId>,
    new_oid: Option<&ObjectId>,
    path: &str,
) -> Result<String, ApiError> {
    let raw = String::from_utf8_lossy(commit.message().as_slice()).into_owned();
    let subject = subject_of(&raw);
    let (a_name, a_email, a_when) = author_parts(commit);
    let old_str = old_oid.map(|o| o.to_string()).unwrap_or_default();
    let new_str = new_oid.map(|o| o.to_string()).unwrap_or_default();

    // Produce the per-file diff for the same path the history entry
    // is about — same renderer surface as the full commit diff API,
    // but scoped to this one file. The frontend timeline inlines it.
    let old_bytes = match old_oid {
        Some(o) => read_blob(repo, o).unwrap_or_default(),
        None => Vec::new(),
    };
    let new_bytes = match new_oid {
        Some(o) => read_blob(repo, o).unwrap_or_default(),
        None => Vec::new(),
    };
    let file_diff = build_file_diff(path, &old_str, &new_str, &old_bytes, &new_bytes);
    Ok(format!(
        "{{\"oid\":\"{commit_oid}\",\"subject\":{},\"author\":{{\"name\":{},\"email\":{},\"when\":{}}},\
         \"change\":\"{change}\",\"old_oid\":{},\"new_oid\":{},\"diff\":{}}}",
        json_string(subject),
        json_string(&a_name),
        json_string(&a_email),
        a_when,
        json_string(&old_str),
        json_string(&new_str),
        file_diff.to_json(),
    ))
}

/// Same per-file diff selection as `diff_trees` — extracted so the
/// file-history endpoint can produce one entry's diff without rerunning
/// the whole tree walk.
fn build_file_diff(
    path: &str,
    old_oid: &str,
    new_oid: &str,
    old_bytes: &[u8],
    new_bytes: &[u8],
) -> FileDiff {
    let old_owned = old_oid.to_string();
    let new_owned = new_oid.to_string();
    if let Some(summary) = alt_diff::structured::summary_for_path(path, old_bytes, new_bytes) {
        let format = match summary.kind {
            alt_diff::structured::StructKind::Json => "json",
            alt_diff::structured::StructKind::Toml => "toml",
        };
        return FileDiff::Structured {
            path: path.to_string(),
            format,
            old_oid: old_owned,
            new_oid: new_owned,
            paths: summary.paths,
        };
    }
    if let Some(summary) = alt_diff::part_aware::summary(old_bytes, new_bytes) {
        let format = match summary.kind {
            alt_diff::part_aware::PartKind::Png => "png",
            alt_diff::part_aware::PartKind::Zip => "zip",
        };
        let (perceptual_distance, hash_old, hash_new) =
            if matches!(summary.kind, alt_diff::part_aware::PartKind::Png) {
                let old_fp = alt_diff::perceptual::fingerprint(old_bytes);
                let new_fp = alt_diff::perceptual::fingerprint(new_bytes);
                let d = alt_diff::perceptual::distance(old_fp, new_fp);
                let ho = old_fp.map(|f| format!("{:016x}", f.hash));
                let hn = new_fp.map(|f| format!("{:016x}", f.hash));
                (d, ho, hn)
            } else {
                (None, None, None)
            };
        let parts =
            enrich_parts_with_text_patches(&summary.kind, summary.parts, old_bytes, new_bytes);
        let document = if matches!(summary.kind, alt_diff::part_aware::PartKind::Zip) {
            build_document_diff(path, old_bytes, new_bytes)
        } else {
            None
        };
        return FileDiff::PartAware {
            path: path.to_string(),
            format,
            old_oid: old_owned,
            new_oid: new_owned,
            old_bytes: old_bytes.len(),
            new_bytes: new_bytes.len(),
            parts,
            perceptual_distance,
            perceptual_hash_old: hash_old,
            perceptual_hash_new: hash_new,
            document,
        };
    }
    if alt_diff::is_binary(old_bytes) || alt_diff::is_binary(new_bytes) {
        return FileDiff::Binary {
            path: path.to_string(),
            old_oid: old_owned,
            new_oid: new_owned,
            old_bytes: old_bytes.len(),
            new_bytes: new_bytes.len(),
        };
    }
    let mut patch = Vec::new();
    write_unified_with_headers(&mut patch, path, old_bytes, new_bytes);
    FileDiff::Text {
        path: path.to_string(),
        old_oid: old_owned,
        new_oid: new_owned,
        patch: String::from_utf8_lossy(&patch).into_owned(),
    }
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

/// Reviewer-friendly content diff for OOXML files. Each variant is the
/// *natural shape* of that format: a paragraph stream for Word, a grid
/// of cells per sheet for Excel. Each gets a frontend renderer
/// designed for that shape, not a one-size-fits-all line list.
pub(crate) enum DocumentDiff {
    Docx { entries: Vec<DocumentEntry> },
    Xlsx { sheets: Vec<SheetGrid> },
}

pub(crate) struct DocumentEntry {
    /// `added` / `removed` / `same`. We don't try to detect "modified"
    /// — the line diff returns adds+removes and the renderer can pair
    /// adjacent rows visually if it wants to.
    pub change: &'static str,
    pub text: String,
}

/// One worksheet rendered as the cells that ever appeared on either
/// side. Each cell carries the change kind and both sides' values so
/// the grid renderer can show "before → after" in place.
pub(crate) struct SheetGrid {
    pub name: String,
    /// Inclusive max column letter ("A", "B", … "AA") across either
    /// side. Used by the renderer to size the table.
    pub max_col: String,
    /// Inclusive max row number across either side.
    pub max_row: u32,
    pub cells: Vec<SheetCell>,
    /// True iff at least one cell on this sheet changed/added/removed.
    pub has_changes: bool,
}

pub(crate) struct SheetCell {
    /// "A1" / "AB12" etc.
    pub cell_ref: String,
    pub col: String,
    pub row: u32,
    pub change: &'static str,
    /// `Some` on the old side; `None` if the cell didn't exist there.
    pub old: Option<String>,
    /// `Some` on the new side; `None` if the cell didn't exist there.
    pub new: Option<String>,
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
        /// Old / new perceptual fingerprint hashes as hex strings.
        /// These are alt-only — git has no perceptual layer.
        perceptual_hash_old: Option<String>,
        perceptual_hash_new: Option<String>,
        /// Reviewer-friendly document content diff for OOXML files:
        /// paragraph-level for `.docx`, cell-level for `.xlsx`. Surfaces
        /// "what changed for a human reading the document," not "which
        /// internal XML part has different bytes." `None` when the file
        /// isn't OOXML or the extractor declined.
        document: Option<DocumentDiff>,
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
                perceptual_hash_old,
                perceptual_hash_new,
                document,
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
                let hash_old = match perceptual_hash_old {
                    Some(h) => json_string(h),
                    None => "null".to_string(),
                };
                let hash_new = match perceptual_hash_new {
                    Some(h) => json_string(h),
                    None => "null".to_string(),
                };
                let doc_json = match document {
                    Some(DocumentDiff::Docx { entries }) => {
                        let items: Vec<String> = entries
                            .iter()
                            .map(|e| {
                                format!(
                                    "{{\"change\":\"{}\",\"text\":{}}}",
                                    e.change,
                                    json_string(&e.text),
                                )
                            })
                            .collect();
                        format!("{{\"kind\":\"docx\",\"entries\":[{}]}}", items.join(","),)
                    }
                    Some(DocumentDiff::Xlsx { sheets }) => {
                        let sheet_jsons: Vec<String> = sheets
                            .iter()
                            .map(|s| {
                                let cell_jsons: Vec<String> = s
                                    .cells
                                    .iter()
                                    .map(|c| {
                                        let old = match &c.old {
                                            Some(v) => json_string(v),
                                            None => "null".to_string(),
                                        };
                                        let new = match &c.new {
                                            Some(v) => json_string(v),
                                            None => "null".to_string(),
                                        };
                                        format!(
                                            "{{\"ref\":{},\"col\":{},\"row\":{},\"change\":\"{}\",\"old\":{},\"new\":{}}}",
                                            json_string(&c.cell_ref),
                                            json_string(&c.col),
                                            c.row,
                                            c.change,
                                            old,
                                            new,
                                        )
                                    })
                                    .collect();
                                format!(
                                    "{{\"name\":{},\"max_col\":{},\"max_row\":{},\"has_changes\":{},\"cells\":[{}]}}",
                                    json_string(&s.name),
                                    json_string(&s.max_col),
                                    s.max_row,
                                    s.has_changes,
                                    cell_jsons.join(","),
                                )
                            })
                            .collect();
                        format!(
                            "{{\"kind\":\"xlsx\",\"sheets\":[{}]}}",
                            sheet_jsons.join(","),
                        )
                    }
                    None => "null".to_string(),
                };
                format!(
                    "{{\"kind\":\"part_aware\",\"path\":{},\"format\":\"{}\",\"old_oid\":{},\"new_oid\":{},\"old_bytes\":{},\"new_bytes\":{},\"perceptual_distance\":{},\"perceptual_hash_old\":{},\"perceptual_hash_new\":{},\"document\":{},\"parts\":[{}]}}",
                    json_string(path),
                    format,
                    json_string(old_oid),
                    json_string(new_oid),
                    old_bytes,
                    new_bytes,
                    pd,
                    hash_old,
                    hash_new,
                    doc_json,
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
            let (perceptual_distance, hash_old, hash_new) =
                if matches!(summary.kind, alt_diff::part_aware::PartKind::Png) {
                    let old_fp = alt_diff::perceptual::fingerprint(&old_bytes);
                    let new_fp = alt_diff::perceptual::fingerprint(&new_bytes);
                    let d = alt_diff::perceptual::distance(old_fp, new_fp);
                    let ho = old_fp.map(|f| format!("{:016x}", f.hash));
                    let hn = new_fp.map(|f| format!("{:016x}", f.hash));
                    (d, ho, hn)
                } else {
                    (None, None, None)
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
            let document = if matches!(summary.kind, alt_diff::part_aware::PartKind::Zip) {
                build_document_diff(path, &old_bytes, &new_bytes)
            } else {
                None
            };

            out.push(FileDiff::PartAware {
                path: path.to_string(),
                format,
                old_oid: old_oid_str,
                new_oid: new_oid_str,
                old_bytes: old_bytes.len(),
                new_bytes: new_bytes.len(),
                parts,
                perceptual_distance,
                perceptual_hash_old: hash_old,
                perceptual_hash_new: hash_new,
                document,
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

/// Cap on the number of OOXML document entries the response carries.
/// A docx with thousands of paragraphs would otherwise dominate the
/// JSON payload; reviewers don't need every single line back.
const MAX_DOC_ENTRIES: usize = 2000;

fn build_document_diff(path: &str, old_bytes: &[u8], new_bytes: &[u8]) -> Option<DocumentDiff> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())?;

    let old_members = alt_diff::part_aware::zip_member_bodies(old_bytes, MAX_PART_BODY)?;
    let new_members = alt_diff::part_aware::zip_member_bodies(new_bytes, MAX_PART_BODY)?;

    match ext.as_str() {
        "docx" | "docm" | "dotx" | "dotm" => {
            let old_xml = old_members.get("word/document.xml")?.as_ref()?;
            let new_xml = new_members.get("word/document.xml")?.as_ref()?;
            let old_paragraphs = crate::ooxml::docx_paragraphs(old_xml);
            let new_paragraphs = crate::ooxml::docx_paragraphs(new_xml);
            let entries = diff_lists(&old_paragraphs, &new_paragraphs);
            if entries.is_empty() {
                None
            } else {
                Some(DocumentDiff::Docx { entries })
            }
        }
        "xlsx" | "xlsm" | "xltx" | "xltm" => {
            let sheets = build_xlsx_sheet_grids(&old_members, &new_members);
            if sheets.iter().all(|s| !s.has_changes) {
                None
            } else {
                Some(DocumentDiff::Xlsx { sheets })
            }
        }
        _ => None,
    }
}

fn build_xlsx_sheet_grids(
    old_members: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
    new_members: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
) -> Vec<SheetGrid> {
    let collect_sheets = |members: &std::collections::BTreeMap<String, Option<Vec<u8>>>| {
        members
            .iter()
            .filter(|(k, _)| k.starts_with("xl/worksheets/sheet"))
            .filter_map(|(k, v)| v.as_ref().map(|b| (k.clone(), b.clone())))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let old_sheets = collect_sheets(old_members);
    let new_sheets = collect_sheets(new_members);
    let old_shared = old_members
        .get("xl/sharedStrings.xml")
        .and_then(|v| v.as_ref().map(|b| b.as_slice()));
    let new_shared = new_members
        .get("xl/sharedStrings.xml")
        .and_then(|v| v.as_ref().map(|b| b.as_slice()));
    let old_workbook = old_members
        .get("xl/workbook.xml")
        .and_then(|v| v.as_ref().map(|b| b.as_slice()));
    let new_workbook = new_members
        .get("xl/workbook.xml")
        .and_then(|v| v.as_ref().map(|b| b.as_slice()));
    let old_cells = crate::ooxml::xlsx_cells(&old_sheets, old_shared, old_workbook);
    let new_cells = crate::ooxml::xlsx_cells(&new_sheets, new_shared, new_workbook);

    let mut old_by_sheet: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, String>,
    > = std::collections::BTreeMap::new();
    for c in &old_cells {
        old_by_sheet
            .entry(c.sheet.clone())
            .or_default()
            .insert(c.cell_ref.clone(), c.value.clone());
    }
    let mut new_by_sheet: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, String>,
    > = std::collections::BTreeMap::new();
    for c in &new_cells {
        new_by_sheet
            .entry(c.sheet.clone())
            .or_default()
            .insert(c.cell_ref.clone(), c.value.clone());
    }

    let mut sheet_names: Vec<String> = old_by_sheet
        .keys()
        .chain(new_by_sheet.keys())
        .cloned()
        .collect();
    sheet_names.sort();
    sheet_names.dedup();

    sheet_names
        .into_iter()
        .map(|name| {
            let old_grid = old_by_sheet.get(&name).cloned().unwrap_or_default();
            let new_grid = new_by_sheet.get(&name).cloned().unwrap_or_default();
            build_one_sheet_grid(name, &old_grid, &new_grid)
        })
        .collect()
}

fn build_one_sheet_grid(
    name: String,
    old: &std::collections::BTreeMap<String, String>,
    new: &std::collections::BTreeMap<String, String>,
) -> SheetGrid {
    let mut refs: Vec<String> = old.keys().chain(new.keys()).cloned().collect();
    refs.sort_by(|a, b| {
        let (ac, ar) = parse_ref(a);
        let (bc, br) = parse_ref(b);
        ar.cmp(&br).then_with(|| ac.cmp(&bc))
    });
    refs.dedup();

    let mut cells = Vec::with_capacity(refs.len());
    let mut max_col_num = 0u32;
    let mut max_row = 0u32;
    let mut has_changes = false;
    for r in refs {
        let (col, row) = parse_ref(&r);
        max_col_num = max_col_num.max(col_letter_to_num(&col));
        max_row = max_row.max(row);
        let o = old.get(&r).cloned();
        let n = new.get(&r).cloned();
        let change = match (&o, &n) {
            (Some(a), Some(b)) if a == b => "same",
            (Some(_), Some(_)) => {
                has_changes = true;
                "changed"
            }
            (None, Some(_)) => {
                has_changes = true;
                "added"
            }
            (Some(_), None) => {
                has_changes = true;
                "removed"
            }
            (None, None) => continue,
        };
        cells.push(SheetCell {
            cell_ref: r.clone(),
            col,
            row,
            change,
            old: o,
            new: n,
        });
    }
    let max_col = if max_col_num > 0 {
        col_num_to_letter(max_col_num)
    } else {
        "A".to_string()
    };
    SheetGrid {
        name,
        max_col,
        max_row,
        cells,
        has_changes,
    }
}

fn parse_ref(s: &str) -> (String, u32) {
    let mut col = String::new();
    let mut row = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() {
            col.push(ch.to_ascii_uppercase());
        } else if ch.is_ascii_digit() {
            row.push(ch);
        }
    }
    let row_n = row.parse::<u32>().unwrap_or(0);
    (col, row_n)
}

fn col_letter_to_num(col: &str) -> u32 {
    let mut n: u32 = 0;
    for ch in col.chars() {
        n = n * 26 + (ch as u32 - 'A' as u32 + 1);
    }
    n
}

fn col_num_to_letter(mut n: u32) -> String {
    let mut s = String::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        s.insert(0, char::from(b'A' + rem as u8));
        n = (n - 1) / 26;
    }
    if s.is_empty() { "A".to_string() } else { s }
}

/// Linear diff over two ordered lists of strings: emit one entry per
/// item, marked added/removed/same. The Myers `diff_lines` output is a
/// list of change-region `Edit` ranges (one with empty `old` is an
/// insert, empty `new` is a delete, both populated is a paired replace);
/// we walk it together with the matching `Same` runs (the gaps between
/// successive `Edit`s) to produce a flat reviewer-friendly stream.
fn diff_lists(old: &[String], new: &[String]) -> Vec<DocumentEntry> {
    let joined_old = old.join("\n");
    let joined_new = new.join("\n");
    let old_lines = alt_diff::split_lines(joined_old.as_bytes());
    let new_lines = alt_diff::split_lines(joined_new.as_bytes());
    let edits = alt_diff::diff_lines(&old_lines, &new_lines);

    let mut out: Vec<DocumentEntry> = Vec::new();
    let mut cursor_old = 0;
    let mut cursor_new = 0;
    let push = |out: &mut Vec<DocumentEntry>, change: &'static str, bytes: &[u8]| -> bool {
        if out.len() >= MAX_DOC_ENTRIES {
            return false;
        }
        let text = trim_trailing_nl(&String::from_utf8_lossy(bytes));
        out.push(DocumentEntry { change, text });
        true
    };

    for edit in &edits {
        // Same lines between the previous edit and this one stay in
        // context — useful for reviewing what's around the change.
        while cursor_old < edit.old.start && cursor_new < edit.new.start {
            if !push(&mut out, "same", old_lines[cursor_old]) {
                return out;
            }
            cursor_old += 1;
            cursor_new += 1;
        }
        for i in edit.old.clone() {
            if !push(&mut out, "removed", old_lines[i]) {
                return out;
            }
        }
        for i in edit.new.clone() {
            if !push(&mut out, "added", new_lines[i]) {
                return out;
            }
        }
        cursor_old = edit.old.end;
        cursor_new = edit.new.end;
    }
    while cursor_old < old_lines.len() && cursor_new < new_lines.len() {
        if !push(&mut out, "same", old_lines[cursor_old]) {
            return out;
        }
        cursor_old += 1;
        cursor_new += 1;
    }
    out
}

fn trim_trailing_nl(s: &str) -> String {
    if let Some(stripped) = s.strip_suffix("\r\n") {
        stripped.to_string()
    } else if let Some(stripped) = s.strip_suffix('\n') {
        stripped.to_string()
    } else {
        s.to_string()
    }
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
