//! The git configuration outside the repository, the system's and the
//! user's, which a repository's own configuration is read on top of, and
//! the attribute files that lie outside the tree.

use std::borrow::Cow;
use std::cell::RefCell;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use alt_bytes::BString;
use alt_git_config::{Config, Entry, IncludeContext};

mod applying;
mod files;

pub(crate) use applying::applying;
pub use applying::forget;

pub(crate) use files::{outer_attributes, read_if_file, user_attributes, user_excludes};

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

/// Variables by name, shared with whoever named them.
type Env = Arc<Vec<(String, String)>>;

thread_local! {
    /// the variables of the process this thread runs a command for
    static CALLER: RefCell<Option<Env>> = const { RefCell::new(None) };
}

/// While it lives, this thread reads the variables of [`CALLER_ENV`]
/// from the ones it was made with and not from the process.
pub struct Caller(Option<Env>);

impl Caller {
    pub fn new(env: impl Into<Env>) -> Self {
        Caller(CALLER.replace(Some(env.into())))
    }
}

impl Drop for Caller {
    fn drop(&mut self) {
        CALLER.set(self.0.take());
    }
}

/// Whether git would match the caller's texts as UTF-8: the first of
/// `LC_ALL`, `LC_CTYPE` and `LANG` that is set names the locale, and the
/// C library must have that locale, with UTF-8 as its character set. A
/// locale that is not installed leaves git in the locale `C`. Which
/// UTF-8 locale it is changes nothing further: the classes and the case
/// of characters are those of `C.UTF-8`, where git would take them from
/// the locale named, the Turkish pairing of the letters i among them.
pub(crate) fn utf8_locale() -> bool {
    let set = |name| var(name).filter(|value| !value.is_empty());
    let Some(locale) = ["LC_ALL", "LC_CTYPE", "LANG"].into_iter().find_map(set) else {
        return false;
    };
    let Ok(locale) = std::ffi::CString::new(locale.into_encoded_bytes()) else {
        return false;
    };
    alt_os::locale::charset(&locale).is_some_and(|charset| charset == b"UTF-8")
}

/// A variable of [`CALLER_ENV`] as the caller has it.
pub(crate) fn var(name: &str) -> Option<OsString> {
    CALLER.with_borrow(|caller| match caller {
        Some(env) => env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| OsString::from(value)),
        None => unnamed(name),
    })
}

#[cfg(not(test))]
fn unnamed(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// A unit test that names no caller has no home, no locale and no
/// system configuration: the machine it runs on has no say in it.
#[cfg(test)]
fn unnamed(name: &str) -> Option<OsString> {
    (name == "GIT_CONFIG_NOSYSTEM").then(|| OsString::from("1"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    var(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub(crate) fn home() -> Option<PathBuf> {
    env_path("HOME")
}

/// The directory the user's git files are in: `$XDG_CONFIG_HOME/git`, or
/// `~/.config/git`.
pub(crate) fn user_dir() -> Option<PathBuf> {
    env_path("XDG_CONFIG_HOME")
        .or_else(|| home().map(|h| h.join(".config")))
        .map(|config| config.join("git"))
}

/// The entries of the system's and the user's configuration, in the
/// order git reads them: `/etc/gitconfig`, `$XDG_CONFIG_HOME/git/config`,
/// `~/.gitconfig`. `GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_SYSTEM` and
/// `GIT_CONFIG_GLOBAL` are honoured as git honours them.
pub(crate) fn outer(git_dir: Option<PathBuf>, branch: Option<BString>) -> Res<Vec<Entry>> {
    Ok(outer_listed(git_dir, branch)?.0)
}

/// [`outer`], with the files the entries were read from or would be
/// once those exist.
pub(crate) fn outer_listed(
    git_dir: Option<PathBuf>,
    branch: Option<BString>,
) -> Res<(Vec<Entry>, Vec<PathBuf>)> {
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
    let mut listed = Vec::new();
    for file in files {
        // as in git, a file the user may not read is passed over
        let denied = |e: std::io::Error| e.kind() == std::io::ErrorKind::PermissionDenied;
        if !file.is_file() || std::fs::File::open(&file).is_err_and(denied) {
            listed.push(file);
            continue;
        }
        let (read, from) = Config::load_listing(&file, &ctx)?;
        entries.extend(read.entries);
        listed.extend(from);
    }
    Ok((entries, listed))
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

/// The repository's own configuration as the caller reads it. A daemon
/// holds a repository open for callers whose home is not its own, and a
/// file the configuration includes from under `~` is the caller's.
pub(crate) fn repository(repo: &alt_repo::Repository) -> Res<Cow<'_, Config>> {
    let ours = std::env::var_os("HOME").map(PathBuf::from);
    let theirs = var("HOME").map(PathBuf::from);
    if theirs == ours {
        return Ok(Cow::Borrowed(repo.config()?));
    }
    Ok(Cow::Owned(repo.config_at(theirs)?))
}

/// A failure that changed nothing, with the exit status git has for it.
pub(crate) fn fatal(message: String) -> Box<dyn std::error::Error> {
    let refusal = crate::machine::Failure::refusal("execution_error", message);
    Box::new(crate::machine::Failure {
        exit_code: 128,
        ..refusal
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_without_a_caller_reads_no_configuration_of_the_machine() {
        assert!(std::env::var_os("HOME").is_some());
        assert_eq!(home(), None);
        assert_eq!(outer(None, None).unwrap().len(), 0);
        assert!(!utf8_locale());
        let named = vec![("HOME".to_owned(), "/nowhere".to_owned())];
        let caller = Caller::new(named);
        assert_eq!(home(), Some(PathBuf::from("/nowhere")));
        drop(caller);
        assert_eq!(home(), None);
    }

    #[test]
    fn the_locale_says_whether_texts_are_utf8() {
        let says = |pairs: &[(&str, &str)]| {
            let env = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string()));
            let _caller = Caller::new(env.collect::<Vec<_>>());
            utf8_locale()
        };
        // the one UTF-8 locale every system of its kind has
        let utf8 = match cfg!(target_os = "linux") {
            true => "C.UTF-8",
            false => "en_US.UTF-8",
        };
        assert!(says(&[("LANG", utf8)]));
        assert!(says(&[("LC_ALL", utf8)]));
        assert!(!says(&[("LANG", "C")]));
        assert!(!says(&[("LANG", "POSIX")]));
        assert!(!says(&[]));
        // a locale the system does not have leaves the texts bytes
        assert!(!says(&[("LC_ALL", "xx_XX.UTF-8")]));
        assert!(!says(&[("LC_CTYPE", "xx_XX.UTF-8"), ("LANG", utf8)]));
        // the first that is set stands, also against the others
        assert!(!says(&[("LC_ALL", "C"), ("LANG", utf8)]));
        assert!(says(&[("LC_ALL", ""), ("LC_CTYPE", utf8), ("LANG", "C")]));
        assert!(!says(&[("LC_CTYPE", "POSIX"), ("LANG", utf8)]));
    }
}
