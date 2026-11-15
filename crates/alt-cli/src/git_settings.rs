//! The git configuration outside the repository, the system's and the
//! user's, which a repository's own configuration is read on top of, and
//! the attribute files that lie outside the tree.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use alt_bytes::BString;
use alt_git_config::{Config, Entry, IncludeContext};
use alt_worktree::attributes::Layer;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// The variables that say where the configuration outside a repository
/// lies and which character set texts are in. A command run for another
/// process reads them from that process.
pub const CALLER_ENV: &[&str] = &[
    "HOME",
    "XDG_CONFIG_HOME",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_GLOBAL",
    "LC_ALL",
    "LC_CTYPE",
    "LANG",
];

thread_local! {
    /// the variables of the process this thread runs a command for
    static CALLER: RefCell<Option<Vec<(String, String)>>> = const { RefCell::new(None) };
}

/// While it lives, this thread reads the variables of [`CALLER_ENV`]
/// from the ones it was made with and not from the process.
pub struct Caller(Option<Vec<(String, String)>>);

impl Caller {
    pub fn new(env: Vec<(String, String)>) -> Self {
        Caller(CALLER.replace(Some(env)))
    }
}

impl Drop for Caller {
    fn drop(&mut self) {
        CALLER.set(self.0.take());
    }
}

/// A variable of [`CALLER_ENV`] as the caller has it.
pub(crate) fn var(name: &str) -> Option<OsString> {
    CALLER.with_borrow(|caller| match caller {
        Some(env) => env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| OsString::from(value)),
        None => std::env::var_os(name),
    })
}

fn env_path(name: &str) -> Option<PathBuf> {
    var(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub(crate) fn home() -> Option<PathBuf> {
    env_path("HOME")
}

/// The directory the user's git files are in: `$XDG_CONFIG_HOME/git`, or
/// `~/.config/git`.
fn user_dir() -> Option<PathBuf> {
    env_path("XDG_CONFIG_HOME")
        .or_else(|| home().map(|h| h.join(".config")))
        .map(|config| config.join("git"))
}

/// The entries of the system's and the user's configuration, in the
/// order git reads them: `/etc/gitconfig`, `$XDG_CONFIG_HOME/git/config`,
/// `~/.gitconfig`. `GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_SYSTEM` and
/// `GIT_CONFIG_GLOBAL` are honoured as git honours them.
pub(crate) fn outer(git_dir: Option<PathBuf>, branch: Option<BString>) -> Res<Vec<Entry>> {
    let ctx = IncludeContext {
        git_dir,
        branch,
        home: home(),
    };
    let no_system = var("GIT_CONFIG_NOSYSTEM").is_some_and(|v| {
        let v = v.to_string_lossy().to_ascii_lowercase();
        matches!(v.as_str(), "true" | "yes" | "on") || v.parse::<i64>().is_ok_and(|n| n != 0)
    });
    let mut files = Vec::new();
    // a variable set to nothing names no file, and no other is read
    // in its place
    if !no_system {
        files.push(match var("GIT_CONFIG_SYSTEM") {
            Some(named) => PathBuf::from(named),
            None => PathBuf::from("/etc/gitconfig"),
        });
    }
    match var("GIT_CONFIG_GLOBAL") {
        Some(only) => files.push(PathBuf::from(only)),
        None => {
            files.extend(user_dir().map(|dir| dir.join("config")));
            files.extend(home().map(|h| h.join(".gitconfig")));
        }
    }
    let mut entries = Vec::new();
    for file in files.iter().filter(|f| f.is_file()) {
        // as in git, a file the user may not read is passed over
        let denied = |e: std::io::Error| e.kind() == std::io::ErrorKind::PermissionDenied;
        if std::fs::File::open(file).is_err_and(denied) {
            continue;
        }
        entries.extend(Config::load(file, &ctx)?.entries);
    }
    Ok(entries)
}

/// `repository`'s configuration on top of the system's and the user's.
pub(crate) fn layered(
    repository: &Config,
    git_dir: Option<PathBuf>,
    branch: Option<BString>,
) -> Res<Config> {
    let mut entries = outer(git_dir, branch)?;
    entries.extend(repository.entries.iter().cloned());
    Ok(Config { entries })
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
fn read_followed(path: PathBuf) -> Res<Option<Vec<u8>>> {
    match path.is_file() {
        true => Ok(Some(std::fs::read(path)?)),
        false => Ok(None),
    }
}

/// The attribute files outside the tree: the user's, at
/// `core.attributesFile` or in the user's git directory, and the
/// repository's own at `info`. Unlike the files in the tree, git reads
/// these through a symlink.
pub(crate) fn outer_attributes(settings: &Config, info: &Path) -> Res<Vec<Layer>> {
    let mut layers = Vec::new();
    let user = match settings.get_str("core", None, "attributesfile") {
        Some(path) => {
            let path = std::str::from_utf8(path).map_err(|_| "core.attributesFile is not UTF-8")?;
            Some(match (path.strip_prefix("~/"), home()) {
                (Some(rest), Some(home)) => home.join(rest),
                (Some(_), None) => {
                    return Err(format!("failed to expand user dir in: '{path}'").into());
                }
                _ => PathBuf::from(path),
            })
        }
        None => user_dir().map(|dir| dir.join("attributes")),
    };
    if let Some(content) = user.map(read_followed).transpose()?.flatten() {
        layers.push(Layer::global(&content));
    }
    if let Some(content) = read_followed(info.to_path_buf())? {
        layers.push(Layer::info(&content));
    }
    Ok(layers)
}
