//! The ignore and attribute files outside the tree, where the
//! configuration and the user's git directory say they are.

use std::path::{Path, PathBuf};

use alt_git_config::Config;
use alt_worktree::attributes::Layer;

use super::{Res, fatal, home, user_dir};

/// The file the setting `key` of the `core` section names, as git
/// reads it: `None` when it is not set, no file when it is set to
/// nothing. A key without a value is an error that names the file and
/// the line the key is on, and so is a home directory that is not known.
fn named_file(settings: &Config, key: &str) -> Res<Option<Option<PathBuf>>> {
    match settings.get("core", None, key) {
        None => Ok(None),
        Some(None) => {
            let missing = format!("error: missing value for 'core.{key}'\n");
            crate::diagnostics::emit(missing.as_bytes())?;
            let origin = settings
                .entry("core", None, key)
                .and_then(|e| e.origin.as_ref());
            let at = origin.map_or_else(String::new, |o| {
                format!(" in file '{}' at line {}", o.file.display(), o.line)
            });
            Err(fatal(format!("bad config variable 'core.{key}'{at}")))
        }
        Some(Some(path)) if path.is_empty() => Ok(Some(None)),
        Some(Some(path)) => Ok(Some(Some(expand_user(path)?))),
    }
}

/// A path from the configuration, as git expands it: `~/` stands for the
/// home directory, `~name/` for that of the user `name`.
fn expand_user(path: &[u8]) -> Res<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let plain = |bytes: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    let Some(named) = path.strip_prefix(b"~") else {
        return Ok(plain(path));
    };
    let (user, rest) = match named.iter().position(|b| *b == b'/') {
        Some(at) => (&named[..at], &named[at + 1..]),
        None => (named, &b""[..]),
    };
    let home = match user.is_empty() {
        true => home(),
        false => alt_os::users::home_of(&std::ffi::CString::new(user)?)?,
    };
    let unknown = || {
        let path = String::from_utf8_lossy(path);
        fatal(format!("failed to expand user dir in: '{path}'"))
    };
    Ok(home.ok_or_else(unknown)?.join(plain(rest)))
}

/// The user's ignore file as it is named: the one `core.excludesFile`
/// names, else the one in the user's git directory. A relative path is
/// one in the working tree.
pub(crate) fn user_excludes(settings: &Config) -> Res<Option<PathBuf>> {
    match named_file(settings, "excludesfile")? {
        Some(named) => Ok(named),
        None => Ok(user_dir().map(|dir| dir.join("ignore"))),
    }
}

/// The user's attributes file: the one `core.attributesFile` names, a
/// relative path being one in the working tree at `root`, else the one
/// in the user's git directory.
pub(crate) fn user_attributes(settings: &Config, root: Option<&Path>) -> Res<Option<PathBuf>> {
    Ok(match named_file(settings, "attributesfile")? {
        Some(named) => named.map(|named| match root {
            Some(root) => root.join(named),
            None => named,
        }),
        None => user_dir().map(|dir| dir.join("attributes")),
    })
}

/// The content of a regular file; `None` when there is no such file. A
/// symlink is not followed, as git does not follow one either.
pub(crate) fn read_if_file(path: PathBuf) -> Res<Option<Vec<u8>>> {
    match path.symlink_metadata() {
        Ok(meta) if meta.is_file() => Ok(Some(std::fs::read(path)?)),
        _ => Ok(None),
    }
}

/// The content of a file, through a symlink too; `None` when there is
/// no such file.
fn read_followed(path: &Path) -> Res<Option<Vec<u8>>> {
    match path.is_file() {
        true => Ok(Some(std::fs::read(path)?)),
        false => Ok(None),
    }
}

/// The attribute files outside the tree: the user's at `user`, and the
/// repository's own at `info`. Unlike the files in the tree, git reads
/// these through a symlink.
pub(crate) fn outer_attributes(user: Option<&Path>, info: &Path) -> Res<Vec<Layer>> {
    let mut layers = Vec::new();
    if let Some(content) = user.map(read_followed).transpose()?.flatten() {
        layers.push(Layer::global(&content));
    }
    if let Some(content) = read_followed(info)? {
        layers.push(Layer::info(&content));
    }
    Ok(layers)
}
