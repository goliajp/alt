//! Repository facade.
//!
//! Domain layer between the storage crates (codec, pack, refs, config,
//! index, native store) and the CLI. Read-only access to repositories —
//! discovery, object reads, rev-parse, revision walking — over either
//! backend: a `.git` directory (M1) or a native `.alt` store (M2).

mod odb;
mod revwalk;

use std::fs;
use std::path::{Path, PathBuf};

use alt_git_codec::{Commit, HashAlgo, LooseStore, ObjectId, ObjectKind, RawObject, Tag, Tree};
use alt_git_config::{Config, IncludeContext};
use alt_git_pack::IndexedPack;
use alt_git_refs::{RefStore, RefTarget};
use alt_odb::NativeOdb;
use bstr::{BString, ByteSlice};

pub use revwalk::RevWalk;

/// Where objects and refs actually come from.
enum Backend {
    Git {
        refs: RefStore,
        loose: LooseStore,
        packs: Vec<IndexedPack>,
    },
    Alt(Box<AltBackend>),
}

struct AltBackend {
    odb: NativeOdb,
    refs: alt_refs::RefStore,
}

pub struct Repository {
    /// The backing store directory: `.git` or `.alt`.
    repo_dir: PathBuf,
    work_tree: Option<PathBuf>,
    algo: HashAlgo,
    config: Config,
    backend: Backend,
}

impl Repository {
    /// Walks up from `start` until a repository is found, like git does.
    /// A native `.alt` store wins over `.git` at the same level (they are
    /// not supposed to coexist; the preference makes detection total).
    pub fn discover(start: &Path) -> Result<Self, RepoError> {
        let start = start.canonicalize()?;
        let mut dir = start.as_path();
        loop {
            let dot_alt = dir.join(".alt");
            if dot_alt.is_dir() {
                return Self::open_alt_dir(dot_alt, Some(dir.to_owned()));
            }
            let dot_git = dir.join(".git");
            if dot_git.is_dir() {
                return Self::open_git_dir(dot_git, Some(dir.to_owned()));
            }
            if dot_git.is_file() {
                // `.git` file: `gitdir: <path>` (submodules, linked worktrees)
                let content = fs::read(&dot_git)?;
                let target = content
                    .trim()
                    .strip_prefix(b"gitdir: ")
                    .ok_or(RepoError::Format("malformed .git file"))?;
                let target = dir.join(
                    target
                        .to_path()
                        .map_err(|_| RepoError::Format("non-path .git target"))?,
                );
                return Self::open_git_dir(target.canonicalize()?, Some(dir.to_owned()));
            }
            // bare repository: the directory itself is the git dir
            if dir.join("HEAD").is_file() && dir.join("objects").is_dir() {
                return Self::open_git_dir(dir.to_owned(), None);
            }
            dir = dir
                .parent()
                .ok_or(RepoError::NotARepository(start.clone()))?;
        }
    }

    fn open_git_dir(git_dir: PathBuf, work_tree: Option<PathBuf>) -> Result<Self, RepoError> {
        if git_dir.join("commondir").is_file() {
            // linked worktrees split HEAD from the shared store; the refs
            // layer cannot represent that yet
            return Err(RepoError::Unsupported("linked worktrees"));
        }
        // bootstrap: extensions.* must be readable before anything else,
        // and includes cannot change them — parse the plain file first
        let config_path = git_dir.join("config");
        let plain = match fs::read(&config_path) {
            Ok(data) => Config {
                entries: alt_git_config::parse_file(&data)?,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.into()),
        };
        let algo = match plain.get_str("extensions", None, "objectformat") {
            None => HashAlgo::Sha1,
            Some(v) if v.as_ref() as &[u8] == b"sha256" => HashAlgo::Sha256,
            Some(_) => return Err(RepoError::Format("unknown extensions.objectFormat")),
        };

        let refs = RefStore::open(&git_dir, algo)?;
        let branch = match refs.read("HEAD")? {
            Some(RefTarget::Symbolic(name)) => name
                .strip_prefix(b"refs/heads/")
                .map(|short| BString::from(short.to_vec())),
            _ => None,
        };
        let config = if config_path.is_file() {
            let ctx = IncludeContext {
                git_dir: Some(git_dir.clone()),
                branch,
                home: std::env::var_os("HOME").map(PathBuf::from),
            };
            Config::load(&config_path, &ctx)?
        } else {
            Config::default()
        };

        let objects = git_dir.join("objects");
        let packs = odb::open_packs(&objects.join("pack"), algo)?;
        Ok(Self {
            repo_dir: git_dir,
            work_tree,
            algo,
            config,
            backend: Backend::Git {
                refs,
                loose: LooseStore::new(&objects),
                packs,
            },
        })
    }

    /// Opens a native `.alt` store. The hash algorithm and config come
    /// from the preserved git config (`git-import/config`, contract 2);
    /// a store without one defaults like a fresh git repository.
    fn open_alt_dir(alt_dir: PathBuf, work_tree: Option<PathBuf>) -> Result<Self, RepoError> {
        let config_path = alt_dir.join("git-import/config");
        let plain = match fs::read(&config_path) {
            Ok(data) => Config {
                entries: alt_git_config::parse_file(&data)?,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.into()),
        };
        let algo = match plain.get_str("extensions", None, "objectformat") {
            None => HashAlgo::Sha1,
            Some(v) if v.as_ref() as &[u8] == b"sha256" => HashAlgo::Sha256,
            Some(_) => return Err(RepoError::Format("unknown extensions.objectFormat")),
        };

        let refs = alt_refs::RefStore::open(&alt_dir)?;
        let branch = match refs.get("HEAD") {
            Some(alt_refs::RefTarget::Symbolic(name)) => name
                .strip_prefix("refs/heads/")
                .map(|short| BString::from(short.as_bytes().to_vec())),
            _ => None,
        };
        let config = if config_path.is_file() {
            // include paths resolve relative to the preserved copy; the
            // original .git is gone by design (no coexistence)
            let ctx = IncludeContext {
                git_dir: Some(alt_dir.join("git-import")),
                branch,
                home: std::env::var_os("HOME").map(PathBuf::from),
            };
            Config::load(&config_path, &ctx)?
        } else {
            Config::default()
        };

        let odb = NativeOdb::open(&alt_dir)?;
        Ok(Self {
            repo_dir: alt_dir,
            work_tree,
            algo,
            config,
            backend: Backend::Alt(Box::new(AltBackend { odb, refs })),
        })
    }

    /// The backing store directory: the `.git` dir for a git repository,
    /// the `.alt` dir for a native one.
    pub fn git_dir(&self) -> &Path {
        &self.repo_dir
    }

    pub fn is_native(&self) -> bool {
        matches!(self.backend, Backend::Alt(_))
    }

    pub fn work_tree(&self) -> Option<&Path> {
        self.work_tree.as_deref()
    }

    pub fn algo(&self) -> HashAlgo {
        self.algo
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Brings a held-open repository up to date with writes other processes
    /// committed since it was opened, so a long-lived reader (the `altd`
    /// daemon) never serves a stale git-layer read. The native backend reuses
    /// the store's catch-up machinery (tail reads under the read lock); the git
    /// backend re-reads its refs and re-scans its pack directory. Config is
    /// loaded once at open and not reloaded here.
    pub fn refresh(&mut self) -> Result<(), RepoError> {
        let repo_dir = &self.repo_dir;
        let algo = self.algo;
        match &mut self.backend {
            Backend::Alt(alt) => {
                alt.odb.refresh()?;
                alt.refs.refresh()?;
            }
            Backend::Git { refs, packs, .. } => {
                *refs = RefStore::open(repo_dir, algo)?;
                *packs = odb::open_packs(&repo_dir.join("objects").join("pack"), algo)?;
            }
        }
        Ok(())
    }

    /// The git-side ref store; None on a native `.alt` repository.
    pub fn git_refs(&self) -> Option<&RefStore> {
        match &self.backend {
            Backend::Git { refs, .. } => Some(refs),
            Backend::Alt(_) => None,
        }
    }

    /// List every ref the repo holds as `(name, resolved oid, symref target
    /// or None)`. Resolution follows symrefs (HEAD → refs/heads/main). The
    /// alt-side returns owned strings so the caller doesn't borrow from
    /// the backend across the iteration; ref counts are typically small
    /// enough that this is fine. M9/W10a: the wire server uses it to
    /// answer `ls-refs`.
    pub fn list_refs(&self) -> Result<Vec<(String, ObjectId, Option<String>)>, RepoError> {
        let mut out = Vec::new();
        match &self.backend {
            Backend::Git { refs, .. } => {
                for r in refs.iter_refs()? {
                    let name = r.name.to_string();
                    let Some(oid) = refs.resolve(&name)? else {
                        continue;
                    };
                    let symref_target = match &r.target {
                        alt_git_refs::RefTarget::Symbolic(b) => Some(b.to_string()),
                        alt_git_refs::RefTarget::Direct(_) => None,
                    };
                    out.push((name, oid, symref_target));
                }
            }
            Backend::Alt(alt) => {
                for (name, target) in alt.refs.iter() {
                    let Some(oid) = alt.refs.resolve(name)? else {
                        continue;
                    };
                    let symref_target = match target {
                        alt_refs::RefTarget::Symbolic(s) => Some(s.clone()),
                        alt_refs::RefTarget::Oid(_) => None,
                    };
                    out.push((name.to_owned(), oid, symref_target));
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Resolves a ref name (following symrefs) on either backend.
    pub fn resolve_ref(&self, name: &str) -> Result<Option<ObjectId>, RepoError> {
        match &self.backend {
            Backend::Git { refs, .. } => Ok(refs.resolve(name)?),
            Backend::Alt(alt) => Ok(alt.refs.resolve(name)?),
        }
    }

    /// Reads any object by id: packs first (bulk), then loose (recent).
    pub fn read_object(&self, oid: &ObjectId) -> Result<Option<RawObject>, RepoError> {
        match &self.backend {
            Backend::Git { packs, loose, .. } => {
                for pack in packs {
                    if let Some(obj) = pack.read(oid)? {
                        return Ok(Some(obj.to_raw()));
                    }
                }
                match loose.read(oid) {
                    Ok(obj) => Ok(Some(obj)),
                    Err(alt_git_codec::LooseError::NotFound(_)) => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
            Backend::Alt(alt) => Ok(alt.odb.get(oid)?),
        }
    }

    /// Physical storage layout for `oid` — `Ok(None)` when the object
    /// is unknown to this repo or when the repo is git-backed (the
    /// view only exists for native alt stores). Native repos
    /// always return `Some` for any known oid.
    pub fn storage_view(&self, oid: &ObjectId) -> Result<Option<alt_odb::StorageView>, RepoError> {
        match &self.backend {
            Backend::Git { .. } => Ok(None),
            Backend::Alt(alt) => Ok(alt.odb.storage_view(oid)?),
        }
    }

    /// Aggregate storage report over every object alt holds for this
    /// repo. `Ok(None)` for git-backed repos.
    pub fn storage_stats(&self) -> Result<Option<alt_odb::StorageStats>, RepoError> {
        match &self.backend {
            Backend::Git { .. } => Ok(None),
            Backend::Alt(alt) => Ok(Some(alt.odb.storage_stats()?)),
        }
    }

    pub fn read_commit(&self, oid: &ObjectId) -> Result<Commit, RepoError> {
        let obj = self
            .read_object(oid)?
            .ok_or(RepoError::MissingObject(*oid))?;
        if obj.kind != ObjectKind::Commit {
            return Err(RepoError::Format("expected a commit object"));
        }
        Ok(Commit::parse(&obj.data)?)
    }

    /// Follows tag objects until a non-tag is reached.
    pub fn peel(&self, oid: ObjectId) -> Result<(ObjectId, RawObject), RepoError> {
        let mut oid = oid;
        loop {
            let obj = self
                .read_object(&oid)?
                .ok_or(RepoError::MissingObject(oid))?;
            if obj.kind != ObjectKind::Tag {
                return Ok((oid, obj));
            }
            oid = Tag::parse(&obj.data)?
                .object()
                .ok_or(RepoError::Format("tag without object header"))?;
        }
    }

    /// Resolves a revision spec: full hex, or a ref name through git's
    /// DWIM lookup order, optionally followed by ancestor operators
    /// `~N` / `^N` (git rev-parse §SPECIFYING REVISIONS).
    ///
    /// Ancestor operators applied left-to-right against the base oid:
    /// - `<rev>~`     == one first-parent step
    /// - `<rev>~N`    == N first-parent steps (N=0 is a no-op)
    /// - `<rev>^`     == first parent (= `<rev>^1`)
    /// - `<rev>^N`    == N-th parent (`^2` picks the merge's second parent)
    /// - `<rev>^0`    == the commit itself, after peeling any tag
    ///
    /// They compose: `HEAD~2^^~3` walks first parent twice, then takes
    /// first parent twice, then first parent three more times.
    pub fn rev_parse(&self, spec: &str) -> Result<Option<ObjectId>, RepoError> {
        let split = spec.find(|c: char| c == '~' || c == '^');
        let (base, suffix) = match split {
            None => (spec, ""),
            Some(i) => (&spec[..i], &spec[i..]),
        };
        let Some(mut oid) = self.resolve_base(base)? else {
            return Ok(None);
        };

        let bytes = suffix.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let op = bytes[i];
            i += 1;
            // Optional numeric count attached to the operator.
            let mut n: usize = 0;
            let mut had_digit = false;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                let Some(next) = n
                    .checked_mul(10)
                    .and_then(|v| v.checked_add((bytes[i] - b'0') as usize))
                else {
                    return Ok(None);
                };
                n = next;
                i += 1;
                had_digit = true;
            }
            let n = if had_digit { n } else { 1 };

            // Peel tags before reading the commit; ancestor operators
            // are commit-relative even when the user named a tag.
            let (peeled, _) = self.peel(oid)?;
            let commit = self.read_commit(&peeled)?;
            oid = match op {
                b'~' => {
                    let mut cur = peeled;
                    let mut cur_commit = commit;
                    for _ in 0..n {
                        let Some(p) = cur_commit.parents().next() else {
                            return Ok(None);
                        };
                        cur = p;
                        cur_commit = self.read_commit(&cur)?;
                    }
                    cur
                }
                b'^' => {
                    if n == 0 {
                        peeled
                    } else {
                        let Some(p) = commit.parents().nth(n - 1) else {
                            return Ok(None);
                        };
                        p
                    }
                }
                _ => return Ok(None),
            };
        }
        Ok(Some(oid))
    }

    /// The non-suffix portion of `rev_parse`: full hex, or a ref name
    /// through git's DWIM lookup order. Pulled out so the suffix walker
    /// can recurse on the base independently of operator parsing.
    fn resolve_base(&self, spec: &str) -> Result<Option<ObjectId>, RepoError> {
        if spec.is_empty() {
            return Ok(None);
        }
        if spec.len() == self.algo.hex_len()
            && let Ok(oid) = ObjectId::from_hex(spec.as_bytes())
        {
            return Ok(Some(oid));
        }
        for candidate in [
            spec.to_owned(),
            format!("refs/{spec}"),
            format!("refs/tags/{spec}"),
            format!("refs/heads/{spec}"),
            format!("refs/remotes/{spec}"),
            format!("refs/remotes/{spec}/HEAD"),
        ] {
            if let Some(oid) = self.resolve_ref(&candidate)? {
                return Ok(Some(oid));
            }
        }
        Ok(None)
    }

    /// Date-ordered history walk from `start` (commit or peelable tag).
    pub fn rev_walk(&self, start: ObjectId) -> Result<RevWalk<'_>, RepoError> {
        let (oid, _) = self.peel(start)?;
        RevWalk::new(self, oid)
    }

    /// Enumerate every object (commit, tree, blob, tag) reachable from
    /// `roots`, excluding everything reachable from `excludes`. The result
    /// is the set of objects that need to ship in a push: `roots` are the
    /// tips the client wants to update to, `excludes` are tips the server
    /// already has (so the closure under `excludes` is what the server can
    /// reconstruct deltas against).
    ///
    /// The returned vector lists each object exactly once with its kind so
    /// a downstream pack writer can stream them without re-reading. Order
    /// is unspecified — `PackWriter` handles plain entries in any order.
    pub fn reachable_objects(
        &self,
        roots: &[ObjectId],
        excludes: &[ObjectId],
    ) -> Result<Vec<(ObjectId, ObjectKind)>, RepoError> {
        use std::collections::HashSet;
        let mut uninteresting: HashSet<ObjectId> = HashSet::new();
        let mut stack: Vec<ObjectId> = excludes.to_vec();
        while let Some(oid) = stack.pop() {
            if !uninteresting.insert(oid) {
                continue;
            }
            self.expand_into(oid, &mut stack)?;
        }

        let mut seen: HashSet<ObjectId> = HashSet::new();
        let mut out: Vec<(ObjectId, ObjectKind)> = Vec::new();
        let mut work: Vec<ObjectId> = roots
            .iter()
            .filter(|o| !uninteresting.contains(o))
            .copied()
            .collect();
        while let Some(oid) = work.pop() {
            if uninteresting.contains(&oid) || !seen.insert(oid) {
                continue;
            }
            let Some(obj) = self.read_object(&oid)? else {
                // a root pointing at a missing object is the caller's bug
                // (push wants must be present locally); skip rather than
                // crash so the caller can surface a precise error
                continue;
            };
            out.push((oid, obj.kind));
            for child in object_children(oid, &obj, self.algo)? {
                if !uninteresting.contains(&child) {
                    work.push(child);
                }
            }
        }
        Ok(out)
    }

    /// Push every child oid of `oid` onto `stack` so that the exclude walk
    /// covers the whole closure under the `excludes` set.
    fn expand_into(&self, oid: ObjectId, stack: &mut Vec<ObjectId>) -> Result<(), RepoError> {
        let Some(obj) = self.read_object(&oid)? else {
            return Ok(());
        };
        for child in object_children(oid, &obj, self.algo)? {
            stack.push(child);
        }
        Ok(())
    }
}

fn object_children(
    oid: ObjectId,
    obj: &RawObject,
    algo: HashAlgo,
) -> Result<Vec<ObjectId>, RepoError> {
    let mut out = Vec::new();
    match obj.kind {
        ObjectKind::Commit => {
            let commit = Commit::parse(&obj.data)?;
            if let Some(t) = commit.tree() {
                out.push(t);
            }
            for p in commit.parents() {
                out.push(p);
            }
        }
        ObjectKind::Tree => {
            let tree = Tree::parse(&obj.data, algo)?;
            for entry in &tree.entries {
                // gitlink (mode 160000) points at a foreign commit — we
                // don't ship it (it's a submodule reference, not in our
                // odb).
                if entry.mode.is_gitlink() {
                    continue;
                }
                out.push(entry.oid);
            }
        }
        ObjectKind::Tag => {
            let tag = Tag::parse(&obj.data)?;
            if let Some(t) = tag.object() {
                out.push(t);
            }
        }
        ObjectKind::Blob => {}
    }
    let _ = oid; // reserved for diagnostics if a future variant needs it
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("not a git repository (or any parent): {0}")]
    NotARepository(PathBuf),
    #[error("object {0} is referenced but missing")]
    MissingObject(ObjectId),
    #[error("{0} are not supported yet")]
    Unsupported(&'static str),
    #[error("repository format: {0}")]
    Format(&'static str),
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Loose(#[from] alt_git_codec::LooseError),
    #[error(transparent)]
    Object(#[from] alt_git_codec::ObjectParseError),
    #[error(transparent)]
    Pack(#[from] alt_git_pack::PackError),
    #[error(transparent)]
    Refs(#[from] alt_git_refs::RefError),
    #[error(transparent)]
    Config(#[from] alt_git_config::ConfigError),
    #[error(transparent)]
    Odb(#[from] alt_odb::OdbError),
    #[error(transparent)]
    NativeRefs(#[from] alt_refs::RefError),
}
