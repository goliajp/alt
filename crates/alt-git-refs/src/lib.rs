//! Git reference storage reading: the files backend (loose refs +
//! `packed-refs`) and the reftable backend, with symref resolution.
//! Business-agnostic stone.

mod packed;
mod reftable;

use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use alt_git_codec::{HashAlgo, ObjectId};
use bstr::{BString, ByteSlice};

/// What a ref points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefTarget {
    Direct(ObjectId),
    /// A symref, e.g. `HEAD` → `refs/heads/main`.
    Symbolic(BString),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub name: BString,
    pub target: RefTarget,
    /// For packed annotated tags: the pre-peeled target commit.
    pub peeled: Option<ObjectId>,
}

/// git's symref hop limit (`SYMREF_MAXDEPTH`).
const MAX_SYMREF_DEPTH: usize = 5;

/// How a repository stores its refs.
enum Backend {
    /// Loose files under `refs/` + a `packed-refs` snapshot.
    Files { packed: Vec<Ref> },
    /// Merged snapshot of the reftable stack (includes `HEAD` and other
    /// root refs as table records).
    Reftable { refs: Vec<Ref> },
}

/// Read access to a repository's refs.
///
/// The backend is detected at open time (`reftable/tables.list` presence).
/// Stored state (`packed-refs`, reftable stack) is snapshotted at open;
/// loose ref files are read per call and take precedence, as in git.
pub struct RefStore {
    git_dir: PathBuf,
    algo: HashAlgo,
    backend: Backend,
}

impl RefStore {
    pub fn open(git_dir: impl Into<PathBuf>, algo: HashAlgo) -> Result<Self, RefError> {
        let git_dir = git_dir.into();
        let backend = if git_dir.join("reftable/tables.list").is_file() {
            Backend::Reftable {
                refs: reftable::read_stack(&git_dir, algo)?,
            }
        } else {
            let packed = match fs::read(git_dir.join("packed-refs")) {
                Ok(data) => packed::parse(&data, algo)?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
                Err(e) => return Err(e.into()),
            };
            Backend::Files { packed }
        };
        Ok(Self {
            git_dir,
            algo,
            backend,
        })
    }

    /// Reads one ref by exact name (`HEAD`, `refs/heads/main`, …) without
    /// following symrefs.
    pub fn read(&self, name: &str) -> Result<Option<RefTarget>, RefError> {
        match &self.backend {
            // in reftable repos the on-disk HEAD file is a compat dummy
            // (`refs/heads/.invalid`); every ref lives in the tables
            Backend::Reftable { refs } => Ok(refs
                .binary_search_by(|r| r.name.as_slice().cmp(name.as_bytes()))
                .ok()
                .map(|i| refs[i].target.clone())),
            Backend::Files { packed } => match fs::read(self.git_dir.join(name)) {
                Ok(data) => Ok(Some(parse_loose(&data, self.algo)?)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(packed
                    .iter()
                    .find(|r| r.name == name)
                    .map(|r| r.target.clone())),
                Err(e) => Err(e.into()),
            },
        }
    }

    /// Resolves `name` to an object id, following symrefs like git
    /// (at most [`MAX_SYMREF_DEPTH`] hops).
    pub fn resolve(&self, name: &str) -> Result<Option<ObjectId>, RefError> {
        let mut current: BString = name.into();
        for _ in 0..=MAX_SYMREF_DEPTH {
            match self.read(
                current
                    .to_str()
                    .map_err(|_| RefError::Format("non-utf8 refname"))?,
            )? {
                None => return Ok(None),
                Some(RefTarget::Direct(oid)) => return Ok(Some(oid)),
                Some(RefTarget::Symbolic(next)) => current = next,
            }
        }
        Err(RefError::SymrefDepth)
    }

    /// All refs under `refs/`, sorted by name — the same set
    /// `git for-each-ref` lists.
    pub fn iter_refs(&self) -> Result<Vec<Ref>, RefError> {
        match &self.backend {
            Backend::Reftable { refs } => Ok(refs
                .iter()
                .filter(|r| r.name.starts_with(b"refs/"))
                .cloned()
                .collect()),
            Backend::Files { packed } => {
                let mut out: Vec<Ref> = Vec::new();
                let refs_root = self.git_dir.join("refs");
                walk_loose(&refs_root, &self.git_dir, self.algo, &mut out)?;

                let mut names: std::collections::HashSet<&[u8]> =
                    out.iter().map(|r| r.name.as_slice()).collect();
                let mut merged = out.clone();
                for r in packed {
                    if !names.contains(r.name.as_slice()) {
                        names.insert(r.name.as_slice());
                        merged.push(r.clone());
                    }
                }
                merged.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(merged)
            }
        }
    }
}

fn walk_loose(
    dir: &Path,
    git_dir: &Path,
    algo: HashAlgo,
    out: &mut Vec<Ref>,
) -> Result<(), RefError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk_loose(&path, git_dir, algo, out)?;
        } else {
            let name: BString = path
                .strip_prefix(git_dir)
                .expect("walked path is under git_dir")
                .as_os_str()
                .as_bytes()
                .into();
            let target = parse_loose(&fs::read(&path)?, algo)?;
            out.push(Ref {
                name,
                target,
                peeled: None,
            });
        }
    }
    Ok(())
}

/// Parses a loose ref file: `<hex>\n` or `ref: <name>\n`.
fn parse_loose(data: &[u8], algo: HashAlgo) -> Result<RefTarget, RefError> {
    let line = data.trim_end();
    if let Some(target) = line.strip_prefix(b"ref: ") {
        return Ok(RefTarget::Symbolic(target.into()));
    }
    Ok(RefTarget::Direct(packed::parse_oid(line, algo)?))
}

#[derive(Debug, thiserror::Error)]
pub enum RefError {
    #[error("io")]
    Io(#[from] io::Error),
    #[error("ref format: {0}")]
    Format(&'static str),
    #[error("symref chain deeper than {MAX_SYMREF_DEPTH}")]
    SymrefDepth,
}
