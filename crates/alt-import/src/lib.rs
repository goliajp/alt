//! `alt import`: full `.git` → `.alt` migration.
//!
//! Every object's canonical bytes go through the native odb (which
//! re-hashes them against their git id — a corrupt source dies here, not
//! at export); all refs plus HEAD land as one atomic ref transaction (the
//! single import op); the source's `config` is preserved byte-for-byte
//! under `git-import/` (compatibility contract 2).
//!
//! Ordering is the crash story: objects are flushed durable *before* the
//! ref op is recorded, so an interrupted import leaves orphaned content at
//! worst, never a ref pointing at missing objects. Re-running converges:
//! object puts dedup, an unchanged ref set records no op at all.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::Path;

use alt_bytes::ByteSlice;
use alt_git_codec::{Commit, HashAlgo, LooseStore, ObjectId, ObjectKind, Tag, Tree};
use alt_git_pack::IndexedPack;
use alt_odb::{NativeOdb, OdbError};
use alt_refs::{RefChange, RefError, RefStore, RefTarget};
use alt_repo::{RepoError, Repository};

#[derive(Debug)]
pub enum ImportError {
    Io(std::io::Error),
    Repo(RepoError),
    Odb(OdbError),
    Refs(RefError),
    Pack(alt_git_pack::PackError),
    Loose(alt_git_codec::LooseError),
    GitRefs(alt_git_refs::RefError),
    Object(alt_git_codec::ObjectParseError),
    NonUtf8Ref(String),
    NotAGitSource,
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => f.write_str("io"),
            Self::Repo(e) => std::fmt::Display::fmt(e, f),
            Self::Odb(e) => std::fmt::Display::fmt(e, f),
            Self::Refs(e) => std::fmt::Display::fmt(e, f),
            Self::Pack(e) => std::fmt::Display::fmt(e, f),
            Self::Loose(e) => std::fmt::Display::fmt(e, f),
            Self::GitRefs(e) => std::fmt::Display::fmt(e, f),
            Self::Object(e) => std::fmt::Display::fmt(e, f),
            Self::NonUtf8Ref(name) => write!(f, "ref name is not utf-8: {name}"),
            Self::NotAGitSource => {
                f.write_str("import source must be a git repository, not a native .alt store")
            }
        }
    }
}

impl std::error::Error for ImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Repo(e) => e.source(),
            Self::Odb(e) => e.source(),
            Self::Refs(e) => e.source(),
            Self::Pack(e) => e.source(),
            Self::Loose(e) => e.source(),
            Self::GitRefs(e) => e.source(),
            Self::Object(e) => e.source(),
            Self::NonUtf8Ref(_) | Self::NotAGitSource => None,
        }
    }
}

impl From<std::io::Error> for ImportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<RepoError> for ImportError {
    fn from(e: RepoError) -> Self {
        Self::Repo(e)
    }
}

impl From<OdbError> for ImportError {
    fn from(e: OdbError) -> Self {
        Self::Odb(e)
    }
}

impl From<RefError> for ImportError {
    fn from(e: RefError) -> Self {
        Self::Refs(e)
    }
}

impl From<alt_git_pack::PackError> for ImportError {
    fn from(e: alt_git_pack::PackError) -> Self {
        Self::Pack(e)
    }
}

impl From<alt_git_codec::LooseError> for ImportError {
    fn from(e: alt_git_codec::LooseError) -> Self {
        Self::Loose(e)
    }
}

impl From<alt_git_refs::RefError> for ImportError {
    fn from(e: alt_git_refs::RefError) -> Self {
        Self::GitRefs(e)
    }
}

impl From<alt_git_codec::ObjectParseError> for ImportError {
    fn from(e: alt_git_codec::ObjectParseError) -> Self {
        Self::Object(e)
    }
}

/// What one import run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Objects enumerated in the source (packed + loose).
    pub objects_seen: u64,
    /// Objects actually written (the rest were already present).
    pub objects_new: u64,
    /// Refs (incl. HEAD) in the source.
    pub refs_seen: usize,
    /// Refs created or moved by this run.
    pub refs_changed: usize,
    /// Same-path versions re-encoded as lineage deltas (blobs + trees).
    pub lineage_deltas: u64,
    /// The subset of `lineage_deltas` that are tree objects.
    pub tree_lineage_deltas: u64,
    /// The subset of `lineage_deltas` that are commit objects.
    pub commit_lineage_deltas: u64,
    /// The import op — None when state was already converged (rerun).
    pub op: Option<alt_refs::OpId>,
}

/// Imports `repo` into the `.alt` directory at `alt_dir` (created if
/// missing). Idempotent: re-running against an unchanged source changes
/// nothing and records no op.
pub fn import_git(
    repo: &Repository,
    alt_dir: &Path,
    actor: &str,
    timestamp_ms: u64,
) -> Result<ImportReport, ImportError> {
    let git_refs = repo.git_refs().ok_or(ImportError::NotAGitSource)?;
    let mut odb = NativeOdb::open(alt_dir)?;
    let mut refs = RefStore::open(alt_dir)?;

    // --- objects: packed first (bulk), then loose ---
    let mut seen = 0u64;
    let before = odb.len() as u64;
    let algo = repo.algo();
    let objects_dir = repo.git_dir().join("objects");

    let pack_dir = objects_dir.join("pack");
    if let Ok(entries) = fs::read_dir(&pack_dir) {
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "pack") {
                let indexed = IndexedPack::open(&path, algo)?;
                let idx = indexed.idx();
                // ascending pack offset: bases before their deltas, the
                // cache-friendly order (same as the verify harness)
                let mut order: Vec<(u64, u32)> = (0..idx.len())
                    .map(|i| (idx.offset_at(i).expect("idx in range"), i))
                    .collect();
                order.sort_unstable();
                for (offset, i) in order {
                    let obj = indexed.read_at(offset)?;
                    odb.put(idx.oid_at(i), obj.kind, &obj.data)?;
                    seen += 1;
                }
            }
        }
    }

    let loose = LooseStore::new(&objects_dir);
    for entry in fs::read_dir(&objects_dir)? {
        let entry = entry?;
        let fanout = entry.file_name();
        let Some(fanout) = fanout.to_str() else {
            continue;
        };
        if fanout.len() != 2 || !fanout.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue; // pack/, info/
        }
        for obj in fs::read_dir(entry.path())? {
            let rest = obj?.file_name();
            let Some(rest) = rest.to_str() else { continue };
            let hex = format!("{fanout}{rest}");
            let Ok(oid) = ObjectId::from_hex(hex.as_bytes()) else {
                continue; // tmp_obj_* and other non-object files
            };
            let raw = loose.read(&oid)?;
            odb.put(oid, raw.kind, &raw.data)?;
            seen += 1;
        }
    }

    // objects must be durable before any ref names them
    odb.flush()?;

    // --- refs + HEAD: one atomic transaction = the import op ---
    let mut wanted: Vec<(String, RefTarget)> = Vec::new();
    if let Some(head) = git_refs.read("HEAD")? {
        wanted.push(("HEAD".to_owned(), convert_target(head)?));
    }
    for r in git_refs.iter_refs()? {
        let name = r
            .name
            .to_str()
            .map_err(|_| ImportError::NonUtf8Ref(r.name.to_str_lossy().into_owned()))?
            .to_owned();
        if name == "HEAD" {
            continue; // already handled
        }
        wanted.push((name, convert_target(r.target)?));
    }

    // --- same-path lineage deltas: storage form only, identity untouched ---
    let tips: Vec<ObjectId> = wanted
        .iter()
        .filter_map(|(_, target)| match target {
            RefTarget::Oid(oid) => Some(*oid),
            RefTarget::Symbolic(_) => None,
        })
        .collect();
    let (lineage_deltas, tree_lineage_deltas, commit_lineage_deltas) =
        lineage_pass(&mut odb, &tips, algo)?;
    odb.flush()?;

    let refs_seen = wanted.len();
    let changes: Vec<RefChange> = wanted
        .into_iter()
        .filter_map(|(name, new)| {
            let old = refs.get(&name).cloned();
            if old.as_ref() == Some(&new) {
                None // already converged
            } else {
                Some(RefChange {
                    name,
                    old,
                    new: Some(new),
                })
            }
        })
        .collect();

    let refs_changed = changes.len();
    let op = if changes.is_empty() {
        None
    } else {
        Some(refs.commit(actor, timestamp_ms, &changes)?)
    };

    // --- compatibility contract 2: preserve the source config verbatim ---
    let config_src = repo.git_dir().join("config");
    if config_src.is_file() {
        let dst_dir = alt_dir.join("git-import");
        fs::create_dir_all(&dst_dir)?;
        fs::copy(&config_src, dst_dir.join("config"))?;
    }

    Ok(ImportReport {
        objects_seen: seen,
        objects_new: odb.len() as u64 - before,
        refs_seen,
        refs_changed,
        lineage_deltas,
        tree_lineage_deltas,
        commit_lineage_deltas,
        op,
    })
}

/// Walks history from the tips (each commit once) and re-encodes each
/// changed file's predecessor as a delta against its successor — the
/// same-path lineage recorded at write time, not guessed at read time.
/// First-parent diffs with subtree pruning: equal tree ids cut the walk.
fn lineage_pass(
    odb: &mut NativeOdb,
    tips: &[ObjectId],
    algo: HashAlgo,
) -> Result<(u64, u64, u64), ImportError> {
    let mut visited: HashSet<ObjectId> = HashSet::new();
    let mut queue: VecDeque<ObjectId> = VecDeque::new();
    for &tip in tips {
        // peel tags down to commits; refs at blobs/trees have no history
        let mut oid = tip;
        while let Some(obj) = odb.get(&oid)? {
            match obj.kind {
                ObjectKind::Tag => match Tag::parse(&obj.data)?.object() {
                    Some(next) => oid = next,
                    None => break,
                },
                ObjectKind::Commit => {
                    if visited.insert(oid) {
                        queue.push_back(oid);
                    }
                    break;
                }
                _ => break,
            }
        }
    }

    let mut deltas = 0u64;
    let mut tree_deltas = 0u64;
    let mut commit_deltas = 0u64;
    let mut edges: Vec<(ObjectId, ObjectId, bool)> = Vec::new();
    while let Some(oid) = queue.pop_front() {
        let Some(obj) = odb.get(&oid)? else { continue };
        let commit = Commit::parse(&obj.data)?;
        let parents: Vec<ObjectId> = commit.parents().collect();
        for &parent in &parents {
            if visited.insert(parent) {
                queue.push_back(parent);
            }
        }
        let (Some(new_tree), Some(&first_parent)) = (commit.tree(), parents.first()) else {
            continue;
        };
        let Some(parent_obj) = odb.get(&first_parent)? else {
            continue;
        };
        let Some(old_tree) = Commit::parse(&parent_obj.data)?.tree() else {
            continue;
        };
        edges.clear();
        diff_trees(odb, old_tree, new_tree, algo, &mut edges)?;
        for (old, new, is_tree) in edges.drain(..) {
            if odb.lineage_delta(&old, &new)? {
                deltas += 1;
                if is_tree {
                    tree_deltas += 1;
                }
            }
        }
        // commit lineage: the older first-parent deltas against this commit
        // (the newer one stays full). Commits share author/committer/message
        // bytes with their parent, so the patch is usually a clear win.
        if odb.lineage_delta(&first_parent, &oid)? {
            deltas += 1;
            commit_deltas += 1;
        }
    }
    Ok((deltas, tree_deltas, commit_deltas))
}

/// Collects same-path lineage edges (old, new, is_tree) for objects whose
/// content changed between two trees: the changed tree pair itself plus,
/// recursively, every changed sub-tree and blob. Tree objects are highly
/// similar across commits, so delta'ing them is the main volume win.
/// Order anomalies or shape changes just skip a pair — a missed edge
/// only costs compression, never correctness.
fn diff_trees(
    odb: &NativeOdb,
    old: ObjectId,
    new: ObjectId,
    algo: HashAlgo,
    edges: &mut Vec<(ObjectId, ObjectId, bool)>,
) -> Result<(), ImportError> {
    if old == new {
        return Ok(());
    }
    let (Some(old_obj), Some(new_obj)) = (odb.get(&old)?, odb.get(&new)?) else {
        return Ok(());
    };
    if old_obj.kind != ObjectKind::Tree || new_obj.kind != ObjectKind::Tree {
        return Ok(());
    }
    // this tree changed: record the tree pair as a lineage edge, then
    // descend for the children that changed within it
    edges.push((old, new, true));
    let old_tree = Tree::parse(&old_obj.data, algo)?;
    let new_tree = Tree::parse(&new_obj.data, algo)?;
    let (mut i, mut j) = (0, 0);
    while i < old_tree.entries.len() && j < new_tree.entries.len() {
        let (oe, ne) = (&old_tree.entries[i], &new_tree.entries[j]);
        match oe.name.cmp(&ne.name) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                if oe.oid != ne.oid {
                    match (oe.mode.object_kind(), ne.mode.object_kind()) {
                        (ObjectKind::Blob, ObjectKind::Blob) => edges.push((oe.oid, ne.oid, false)),
                        (ObjectKind::Tree, ObjectKind::Tree) => {
                            diff_trees(odb, oe.oid, ne.oid, algo, edges)?
                        }
                        _ => {}
                    }
                }
                i += 1;
                j += 1;
            }
        }
    }
    Ok(())
}

fn convert_target(target: alt_git_refs::RefTarget) -> Result<RefTarget, ImportError> {
    Ok(match target {
        alt_git_refs::RefTarget::Direct(oid) => RefTarget::Oid(oid),
        alt_git_refs::RefTarget::Symbolic(name) => RefTarget::Symbolic(
            name.to_str()
                .map_err(|_| ImportError::NonUtf8Ref(name.to_str_lossy().into_owned()))?
                .to_owned(),
        ),
    })
}
