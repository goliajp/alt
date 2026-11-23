// CARVE-OUT: pre-existing oversized module; split pieces out as they are touched
//! Repository facade.
//!
//! Domain layer between the storage crates (codec, pack, refs, config,
//! index, native store) and the CLI. Read-only access to repositories —
//! discovery, object reads, rev-parse, revision walking — over either
//! backend: a `.git` directory or a native `.alt` store.

mod alt_backend;
mod object_format;
mod odb;
mod reach;
mod revwalk;
mod ssh_url;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use alt_backend::AltBackend;
use alt_bytes::{BString, ByteSlice};
use alt_git_codec::{Commit, HashAlgo, LooseStore, ObjectId, ObjectKind, RawObject, Tag};
use alt_git_config::{Config, IncludeContext};
use alt_git_pack::IndexedPack;
use alt_git_refs::{RefStore, RefTarget};
pub use object_format::native_object_format;
use object_format::object_format;
use ssh_url::builtin_ssh_to_https;

pub use reach::Ends;
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

pub struct Repository {
    /// The backing store directory: `.git` or `.alt`.
    repo_dir: PathBuf,
    work_tree: Option<PathBuf>,
    /// set on open for a git repository; a native store reads the config
    /// that says it only when something asks
    algo: OnceLock<HashAlgo>,
    /// set on open for a git repository; a native store reads it on first
    /// use
    config: OnceLock<Config>,
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
        let algo = object_format(&config_path)?;

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
            algo: OnceLock::from(algo),
            config: OnceLock::from(config),
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
    /// Opens the `.alt` store at `alt_dir` directly, for callers that have
    /// already resolved which repository (and which workspace working tree)
    /// applies — a named workspace's tree holds only a marker file, which
    /// [`Self::discover`] does not follow.
    pub fn open_alt(alt_dir: PathBuf, work_tree: Option<PathBuf>) -> Result<Self, RepoError> {
        Self::open_alt_dir(alt_dir, work_tree)
    }

    fn open_alt_dir(alt_dir: PathBuf, work_tree: Option<PathBuf>) -> Result<Self, RepoError> {
        let backend = AltBackend::at(&alt_dir);
        Ok(Self {
            repo_dir: alt_dir,
            work_tree,
            algo: OnceLock::new(),
            config: OnceLock::new(),
            backend: Backend::Alt(Box::new(backend)),
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

    /// The hash algorithm of the repository's object ids.
    pub fn algo(&self) -> Result<HashAlgo, RepoError> {
        if let Some(algo) = self.algo.get() {
            return Ok(*algo);
        }
        let algo = object_format(&self.repo_dir.join("git-import/config"))?;
        Ok(*self.algo.get_or_init(|| algo))
    }

    /// Apply git's `[url "X"] insteadOf = Y` rewrite rules to `url`.
    /// Longest matching `insteadOf` prefix wins (same precedence rule
    /// git uses). When nothing matches, `url` comes back unchanged.
    ///
    /// This is how alt accepts the SSH-style URLs (`git@github.com:…`)
    /// that alt-wire-http itself can't parse: configure
    ///
    /// ```text
    /// [url "https://github.com/"]
    ///     insteadOf = git@github.com:
    /// ```
    ///
    /// in the repo's git config (or `~/.gitconfig`) and the SSH URL
    /// is rewritten to HTTPS before the transport ever sees it.
    pub fn rewrite_url(&self, url: &str) -> Result<String, RepoError> {
        let mut best: Option<(usize, String, String)> = None;
        for entry in &self.config()?.entries {
            if entry.section != "url" {
                continue;
            }
            if entry.key != "insteadof" {
                continue;
            }
            let Some(sub) = &entry.subsection else {
                continue;
            };
            let Some(value) = &entry.value else {
                continue;
            };
            let trigger = String::from_utf8_lossy(value.as_ref()).into_owned();
            if !url.starts_with(&trigger) {
                continue;
            }
            let replacement = String::from_utf8_lossy(sub.as_ref()).into_owned();
            let len = trigger.len();
            if best.as_ref().map(|(l, _, _)| len > *l).unwrap_or(true) {
                best = Some((len, trigger, replacement));
            }
        }
        if let Some((tlen, _trigger, replacement)) = best {
            return Ok(format!("{replacement}{}", &url[tlen..]));
        }
        if let Some(rewritten) = builtin_ssh_to_https(url) {
            return Ok(rewritten);
        }
        Ok(url.to_owned())
    }

    pub fn rewrite_url_static(url: &str) -> Option<String> {
        builtin_ssh_to_https(url)
    }

    pub fn config(&self) -> Result<&Config, RepoError> {
        if let Some(config) = self.config.get() {
            return Ok(config);
        }
        let config = match &self.backend {
            Backend::Alt(alt) => alt.load_config(std::env::var_os("HOME").map(PathBuf::from))?,
            Backend::Git { .. } => Config::default(),
        };
        Ok(self.config.get_or_init(|| config))
    }

    /// Brings a held-open repository up to date with writes other processes
    /// committed since it was opened, so a long-lived reader (the `altd`
    /// daemon) never serves a stale git-layer read. The native backend reuses
    /// the store's catch-up machinery (tail reads under the read lock); the git
    /// backend re-reads its refs and re-scans its pack directory. Config is
    /// loaded once at open and not reloaded here.
    pub fn refresh(&mut self) -> Result<(), RepoError> {
        let repo_dir = &self.repo_dir;
        match &mut self.backend {
            Backend::Alt(alt) => {
                // refs before objects: a writer stores its objects before
                // the ref that names them, so refs read first never name
                // an object the refreshed store lacks
                if let Some(refs) = alt.refs.get_mut() {
                    refs.refresh()?;
                }
                alt.odb.refresh()?;
            }
            Backend::Git { refs, packs, .. } => {
                let algo = object_format(&repo_dir.join("config"))?;
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
    /// enough that this is fine. The wire server uses it to
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
                let refs = alt.refs()?;
                for (name, target) in refs.iter() {
                    let Some(oid) = refs.resolve(name)? else {
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
    /// The short name of the branch the symbolic ref `head` names, if
    /// it names one.
    pub fn branch_of(&self, head: &str) -> Result<Option<BString>, RepoError> {
        Ok(match &self.backend {
            Backend::Git { refs, .. } => match refs.read(head)? {
                Some(RefTarget::Symbolic(name)) => name
                    .strip_prefix(b"refs/heads/")
                    .map(|short| BString::from(short.to_vec())),
                _ => None,
            },
            Backend::Alt(alt) => match alt.refs()?.get(head) {
                Some(alt_refs::RefTarget::Symbolic(name)) => name
                    .strip_prefix("refs/heads/")
                    .map(|short| BString::from(short.as_bytes().to_vec())),
                _ => None,
            },
        })
    }

    pub fn resolve_ref(&self, name: &str) -> Result<Option<ObjectId>, RepoError> {
        match &self.backend {
            Backend::Git { refs, .. } => Ok(refs.resolve(name)?),
            Backend::Alt(alt) => Ok(alt.refs()?.resolve(name)?),
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
            Backend::Alt(alt) => Ok(alt.odb()?.get(oid)?),
        }
    }

    /// Physical storage layout for `oid` — `Ok(None)` when the object
    /// is unknown to this repo or when the repo is git-backed (the
    /// view only exists for native alt stores). Native repos
    /// always return `Some` for any known oid.
    pub fn storage_view(&self, oid: &ObjectId) -> Result<Option<alt_odb::StorageView>, RepoError> {
        match &self.backend {
            Backend::Git { .. } => Ok(None),
            Backend::Alt(alt) => Ok(alt.odb()?.storage_view(oid)?),
        }
    }

    /// Aggregate storage report over every object alt holds for this
    /// repo. `Ok(None)` for git-backed repos.
    pub fn storage_stats(&self) -> Result<Option<alt_odb::StorageStats>, RepoError> {
        match &self.backend {
            Backend::Git { .. } => Ok(None),
            Backend::Alt(alt) => Ok(Some(alt.odb()?.storage_stats()?)),
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
        let split = spec.find(['~', '^']);
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
        if let Ok(oid) = ObjectId::from_hex(spec.as_bytes())
            && oid.algo() == self.algo()?
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
        // Short hex prefix as the final DWIM step: ≥ 4 hex chars, less
        // than the full hex length (the full-hex case is already handled
        // above), and unique within the odb. Returns
        // AmbiguousShortOid when multiple objects match, so the caller
        // surfaces a clear message instead of a silent miss.
        if spec.len() >= 4
            && spec.chars().all(|c| c.is_ascii_hexdigit())
            && spec.len() < self.algo()?.hex_len()
            && let Some(oid) = self.resolve_short_oid(spec)?
        {
            return Ok(Some(oid));
        }
        Ok(None)
    }

    fn resolve_short_oid(&self, prefix: &str) -> Result<Option<ObjectId>, RepoError> {
        match &self.backend {
            Backend::Alt(alt) => {
                let matches = alt.odb()?.lookup_by_prefix(prefix, 4)?;
                if matches.is_empty() {
                    Ok(None)
                } else if matches.len() == 1 {
                    Ok(Some(matches[0].git))
                } else {
                    Err(RepoError::AmbiguousShortOid {
                        prefix: prefix.to_owned(),
                        matches: matches.iter().map(|m| m.git).collect(),
                    })
                }
            }
            // Git backend short-oid lookup needs a pack-index walk +
            // a loose-objects directory scan; not wired yet so we return
            // None (the caller falls through to "unknown revision").
            Backend::Git { .. } => Ok(None),
        }
    }

    /// Date-ordered history walk from `start` (commit or peelable tag).
    pub fn rev_walk(&self, start: ObjectId) -> Result<RevWalk<'_>, RepoError> {
        let (oid, _) = self.peel(start)?;
        RevWalk::new(self, oid)
    }
}

#[derive(Debug)]
pub enum RepoError {
    NotARepository(PathBuf),
    MissingObject(ObjectId),
    Unsupported(&'static str),
    Format(&'static str),
    /// A short oid prefix matched more than one object — the caller has
    /// to commit to a longer prefix to disambiguate. Carries up to four
    /// example matches so the error message can list them.
    AmbiguousShortOid {
        prefix: String,
        matches: Vec<ObjectId>,
    },
    Io(std::io::Error),
    Loose(alt_git_codec::LooseError),
    Object(alt_git_codec::ObjectParseError),
    Pack(alt_git_pack::PackError),
    Refs(alt_git_refs::RefError),
    Config(alt_git_config::ConfigError),
    Odb(alt_odb::OdbError),
    NativeRefs(alt_refs::RefError),
}

impl std::fmt::Display for RepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARepository(path) => write!(
                f,
                "not a git repository (or any parent): {}",
                path.display()
            ),
            Self::MissingObject(oid) => write!(f, "object {oid} is referenced but missing"),
            Self::Unsupported(what) => write!(f, "{what} are not supported yet"),
            Self::Format(what) => write!(f, "repository format: {what}"),
            Self::AmbiguousShortOid { prefix, matches } => write!(
                f,
                "short oid '{prefix}' is ambiguous: matches {} objects",
                matches.len()
            ),
            Self::Io(_) => f.write_str("io"),
            Self::Loose(e) => std::fmt::Display::fmt(e, f),
            Self::Object(e) => std::fmt::Display::fmt(e, f),
            Self::Pack(e) => std::fmt::Display::fmt(e, f),
            Self::Refs(e) => std::fmt::Display::fmt(e, f),
            Self::Config(e) => std::fmt::Display::fmt(e, f),
            Self::Odb(e) => std::fmt::Display::fmt(e, f),
            Self::NativeRefs(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for RepoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Loose(e) => e.source(),
            Self::Object(e) => e.source(),
            Self::Pack(e) => e.source(),
            Self::Refs(e) => e.source(),
            Self::Config(e) => e.source(),
            Self::Odb(e) => e.source(),
            Self::NativeRefs(e) => e.source(),
            Self::NotARepository(_)
            | Self::MissingObject(_)
            | Self::Unsupported(_)
            | Self::Format(_)
            | Self::AmbiguousShortOid { .. } => None,
        }
    }
}

impl From<std::io::Error> for RepoError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<alt_git_codec::LooseError> for RepoError {
    fn from(e: alt_git_codec::LooseError) -> Self {
        Self::Loose(e)
    }
}

impl From<alt_git_codec::ObjectParseError> for RepoError {
    fn from(e: alt_git_codec::ObjectParseError) -> Self {
        Self::Object(e)
    }
}

impl From<alt_git_pack::PackError> for RepoError {
    fn from(e: alt_git_pack::PackError) -> Self {
        Self::Pack(e)
    }
}

impl From<alt_git_refs::RefError> for RepoError {
    fn from(e: alt_git_refs::RefError) -> Self {
        Self::Refs(e)
    }
}

impl From<alt_git_config::ConfigError> for RepoError {
    fn from(e: alt_git_config::ConfigError) -> Self {
        Self::Config(e)
    }
}

impl From<alt_odb::OdbError> for RepoError {
    fn from(e: alt_odb::OdbError) -> Self {
        Self::Odb(e)
    }
}

impl From<alt_refs::RefError> for RepoError {
    fn from(e: alt_refs::RefError) -> Self {
        Self::NativeRefs(e)
    }
}
