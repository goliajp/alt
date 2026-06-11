//! Repository facade.
//!
//! Domain layer between the storage stones (codec, pack, refs, config,
//! index) and the CLI. M1 scope: read-only access to existing git
//! repositories — discovery, object reads, rev-parse, revision walking.

mod odb;
mod revwalk;

use std::fs;
use std::path::{Path, PathBuf};

use alt_git_codec::{Commit, HashAlgo, LooseStore, ObjectId, ObjectKind, RawObject, Tag};
use alt_git_config::{Config, IncludeContext};
use alt_git_pack::IndexedPack;
use alt_git_refs::{RefStore, RefTarget};
use bstr::{BString, ByteSlice};

pub use revwalk::RevWalk;

pub struct Repository {
    git_dir: PathBuf,
    work_tree: Option<PathBuf>,
    algo: HashAlgo,
    config: Config,
    refs: RefStore,
    loose: LooseStore,
    packs: Vec<IndexedPack>,
}

impl Repository {
    /// Walks up from `start` until a repository is found, like git does.
    pub fn discover(start: &Path) -> Result<Self, RepoError> {
        let start = start.canonicalize()?;
        let mut dir = start.as_path();
        loop {
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
            git_dir,
            work_tree,
            algo,
            config,
            refs,
            loose: LooseStore::new(&objects),
            packs,
        })
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
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

    pub fn refs(&self) -> &RefStore {
        &self.refs
    }

    /// Reads any object by id: packs first (bulk), then loose (recent).
    pub fn read_object(&self, oid: &ObjectId) -> Result<Option<RawObject>, RepoError> {
        for pack in &self.packs {
            if let Some(obj) = pack.read(oid)? {
                return Ok(Some(obj.to_raw()));
            }
        }
        match self.loose.read(oid) {
            Ok(obj) => Ok(Some(obj)),
            Err(alt_git_codec::LooseError::NotFound(_)) => Ok(None),
            Err(e) => Err(e.into()),
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
    /// DWIM lookup order.
    pub fn rev_parse(&self, spec: &str) -> Result<Option<ObjectId>, RepoError> {
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
            if let Some(oid) = self.refs.resolve(&candidate)? {
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
}
