//! Git reference storage reading, files backend: loose refs under `refs/`,
//! `packed-refs`, and symref resolution. Business-agnostic stone.

mod packed;

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

/// Read access to a repository's refs (files backend).
///
/// `packed-refs` is snapshotted at open time; loose refs are read per call
/// and take precedence, as in git.
pub struct RefStore {
    git_dir: PathBuf,
    algo: HashAlgo,
    packed: Vec<Ref>,
}

impl RefStore {
    pub fn open(git_dir: impl Into<PathBuf>, algo: HashAlgo) -> Result<Self, RefError> {
        let git_dir = git_dir.into();
        let packed = match fs::read(git_dir.join("packed-refs")) {
            Ok(data) => packed::parse(&data, algo)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            git_dir,
            algo,
            packed,
        })
    }

    /// Reads one ref by exact name (`HEAD`, `refs/heads/main`, …) without
    /// following symrefs. Loose beats packed.
    pub fn read(&self, name: &str) -> Result<Option<RefTarget>, RefError> {
        match fs::read(self.git_dir.join(name)) {
            Ok(data) => Ok(Some(parse_loose(&data, self.algo)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(self
                .packed
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.target.clone())),
            Err(e) => Err(e.into()),
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

    /// All refs under `refs/`, loose and packed merged (loose wins),
    /// sorted by name — the same set `git for-each-ref` lists.
    pub fn iter_refs(&self) -> Result<Vec<Ref>, RefError> {
        let mut out: Vec<Ref> = Vec::new();
        let refs_root = self.git_dir.join("refs");
        walk_loose(&refs_root, &self.git_dir, self.algo, &mut out)?;

        let mut names: std::collections::HashSet<&[u8]> =
            out.iter().map(|r| r.name.as_slice()).collect();
        let mut merged = out.clone();
        for r in &self.packed {
            if !names.contains(r.name.as_slice()) {
                names.insert(r.name.as_slice());
                merged.push(r.clone());
            }
        }
        merged.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(merged)
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
