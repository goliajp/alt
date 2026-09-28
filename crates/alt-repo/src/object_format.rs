//! The hash algorithm a repository uses, from `extensions.objectFormat`.

use std::fs;
use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_git_config::Config;

use crate::RepoError;

/// Reads the plain config file: extensions.* must be readable before
/// anything else, and includes cannot change them. A missing file is sha1.
pub(crate) fn object_format(config_path: &Path) -> Result<HashAlgo, RepoError> {
    let plain = match fs::read(config_path) {
        Ok(data) => Config {
            entries: alt_git_config::parse_file(&data)?,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(e.into()),
    };
    match plain.get_str("extensions", None, "objectformat") {
        None => Ok(HashAlgo::Sha1),
        Some(v) if v.as_ref() as &[u8] == b"sha256" => Ok(HashAlgo::Sha256),
        Some(_) => Err(RepoError::Format("unknown extensions.objectFormat")),
    }
}

/// The hash algorithm of a native store: the one of the git repository it
/// was imported from (its config is kept under `git-import/`), else sha1.
pub fn native_object_format(alt_dir: &Path) -> Result<HashAlgo, RepoError> {
    object_format(&alt_dir.join("git-import/config"))
}
