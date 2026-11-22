//! The native `.alt` store behind a [`crate::Repository`], opened piece by
//! piece as a command first needs each one.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use alt_bytes::BString;
use alt_git_config::{Config, IncludeContext};
use alt_odb::NativeOdb;

use crate::{Backend, RepoError, Repository};

pub(crate) struct AltBackend {
    pub alt_dir: PathBuf,
    /// opens its files as reads first need them: a command that only
    /// looks at refs never touches the object store. Read through
    /// [`Self::odb`]
    pub odb: NativeOdb,
    /// opened by the first read of a ref
    pub refs: OnceLock<alt_refs::RefStore>,
    /// the command this serves only reads: no file of the store is
    /// created, repaired or cut when one is opened
    pub reading: bool,
    /// what the ops of the log that are no ref transactions name, for a
    /// command that only reads to leave out what a writer would cut
    pub also: Option<alt_refs::AlsoNamed>,
}

impl AltBackend {
    /// The store under `alt_dir`, nothing of it opened yet.
    pub fn at(alt_dir: &Path) -> Self {
        Self {
            alt_dir: alt_dir.to_owned(),
            odb: NativeOdb::deferred(alt_dir),
            refs: OnceLock::new(),
            reading: false,
            also: None,
        }
    }

    /// The object store, for reading. The refs are opened before it is:
    /// a writer stores its objects before the ref that names them, so
    /// refs read no later than the objects never name one that this view
    /// of the store lacks.
    pub fn odb(&self) -> Result<&NativeOdb, RepoError> {
        self.refs()?;
        Ok(&self.odb)
    }

    pub fn refs(&self) -> Result<&alt_refs::RefStore, RepoError> {
        if let Some(refs) = self.refs.get() {
            return Ok(refs);
        }
        let refs = match self.reading {
            // a store that cannot be asked is reported by whoever reads it
            true => {
                let stored = self.odb.stored_past_cuts();
                let held = |oid: &alt_git_codec::ObjectId| stored(oid).unwrap_or(true);
                alt_refs::RefStore::open_confirmed(&self.alt_dir, self.also.as_ref(), held)?
            }
            false => alt_refs::RefStore::open(&self.alt_dir)?,
        };
        Ok(self.refs.get_or_init(|| refs))
    }

    /// The preserved git config (`git-import/config`), with its includes;
    /// one of a file under `~` is looked for in `home`.
    pub fn load_config(&self, home: Option<PathBuf>) -> Result<Config, RepoError> {
        let config_path = self.alt_dir.join("git-import/config");
        if !config_path.is_file() {
            return Ok(Config::default());
        }
        let branch = match self.refs()?.get("HEAD") {
            Some(alt_refs::RefTarget::Symbolic(name)) => name
                .strip_prefix("refs/heads/")
                .map(|short| BString::from(short.as_bytes().to_vec())),
            _ => None,
        };
        // include paths resolve relative to the preserved copy; the
        // original .git is gone by design (no coexistence)
        let ctx = IncludeContext {
            git_dir: Some(self.alt_dir.join("git-import")),
            branch,
            home,
        };
        Ok(Config::load(&config_path, &ctx)?)
    }
}

impl Repository {
    /// The repository's own configuration as a process whose home is
    /// `home` reads it: a file it includes from under `~` is looked for
    /// there. Read anew at each call; [`config`](Self::config) keeps the
    /// one this process reads.
    pub fn config_at(&self, home: Option<PathBuf>) -> Result<Config, RepoError> {
        match &self.backend {
            Backend::Alt(alt) => alt.load_config(home),
            Backend::Git { .. } => {
                let path = self.repo_dir.join("config");
                if !path.is_file() {
                    return Ok(Config::default());
                }
                let ctx = IncludeContext {
                    git_dir: Some(self.repo_dir.clone()),
                    branch: self.branch_of("HEAD")?,
                    home,
                };
                Ok(Config::load(&path, &ctx)?)
            }
        }
    }

    /// The same repository for a command that only reads. Of a native
    /// store nothing is opened yet: what is opened from here on is opened
    /// to be read, and no file of it changes but its caches, a ref
    /// snapshot that fell behind and index runs that are missing.
    pub fn reading(mut self) -> Self {
        if let Backend::Alt(alt) = &mut self.backend {
            alt.odb = NativeOdb::reading(&alt.alt_dir);
            alt.refs = OnceLock::new();
            alt.reading = true;
        }
        self
    }

    /// Has the refs, when a command that only reads opens them, end
    /// before an op that was never synced and of which `also` names an
    /// object that is not stored, as before a ref transaction of that kind.
    pub fn naming(mut self, also: alt_refs::AlsoNamed) -> Self {
        if let Backend::Alt(alt) = &mut self.backend {
            alt.also = Some(also);
        }
        self
    }

    /// [`Self::refresh`] for a repository held across commands, before
    /// one that only reads: a native store takes in what other writers
    /// appended and writes nothing.
    pub fn follow(&mut self) -> Result<(), RepoError> {
        let Backend::Alt(alt) = &mut self.backend else {
            return self.refresh();
        };
        // refs before objects, as in a refresh
        if let Some(refs) = alt.refs.get_mut() {
            refs.follow()?;
        }
        Ok(alt.odb.follow()?)
    }
}
