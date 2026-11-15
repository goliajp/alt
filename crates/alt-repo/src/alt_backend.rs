//! The native `.alt` store behind a [`crate::Repository`], opened piece by
//! piece as a command first needs each one.

use std::path::PathBuf;
use std::sync::OnceLock;

use alt_bytes::BString;
use alt_git_config::{Config, IncludeContext};
use alt_odb::NativeOdb;

use crate::RepoError;

pub(crate) struct AltBackend {
    pub alt_dir: PathBuf,
    /// opened by the first read of an object: a command that only looks at
    /// refs never touches the object store
    pub odb: OnceLock<NativeOdb>,
    /// opened by the first read of a ref
    pub refs: OnceLock<alt_refs::RefStore>,
}

impl AltBackend {
    pub fn refs(&self) -> Result<&alt_refs::RefStore, RepoError> {
        if let Some(refs) = self.refs.get() {
            return Ok(refs);
        }
        let refs = alt_refs::RefStore::open(&self.alt_dir)?;
        Ok(self.refs.get_or_init(|| refs))
    }

    /// The preserved git config (`git-import/config`), with its includes.
    pub fn load_config(&self) -> Result<Config, RepoError> {
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
            home: std::env::var_os("HOME").map(PathBuf::from),
        };
        Ok(Config::load(&config_path, &ctx)?)
    }

    pub fn odb(&self) -> Result<&NativeOdb, RepoError> {
        if let Some(odb) = self.odb.get() {
            return Ok(odb);
        }
        let odb = NativeOdb::open(&self.alt_dir)?;
        Ok(self.odb.get_or_init(|| odb))
    }
}
