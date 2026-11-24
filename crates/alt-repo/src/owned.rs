//! Whose a repository is, and the git configuration outside it that may
//! name it safe. The configuration of a repository names programs that
//! are run and files that are read, so one that belongs to another user
//! is not opened, as by git, unless the configuration of the user or of
//! the system names it in `safe.directory`.

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use alt_bytes::BString;
use alt_git_config::{Config, Entry, IncludeContext};

use crate::{RepoError, Repository};

/// The variables of whoever a command runs for, by name: where the
/// configuration outside a repository lies is read from them.
pub type Env<'e> = &'e dyn Fn(&str) -> Option<OsString>;

/// The variables of this process.
pub fn process(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

fn env_path(env: Env, name: &str) -> Option<PathBuf> {
    env(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The home of the user.
pub fn home(env: Env) -> Option<PathBuf> {
    env_path(env, "HOME")
}

/// The directory the user's git files are in: `$XDG_CONFIG_HOME/git`, or
/// `~/.config/git`.
pub fn user_dir(env: Env) -> Option<PathBuf> {
    env_path(env, "XDG_CONFIG_HOME")
        .or_else(|| home(env).map(|h| h.join(".config")))
        .map(|config| config.join("git"))
}

/// The entries of the system's and the user's configuration, in the
/// order git reads them: `/etc/gitconfig`, `$XDG_CONFIG_HOME/git/config`,
/// `~/.gitconfig`, with the files they were read from or would be once
/// those exist. `GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_SYSTEM` and
/// `GIT_CONFIG_GLOBAL` are honoured as git honours them.
pub fn outer_listed(
    env: Env,
    git_dir: Option<PathBuf>,
    branch: Option<BString>,
) -> Result<(Vec<Entry>, Vec<PathBuf>), RepoError> {
    let ctx = IncludeContext {
        git_dir,
        branch,
        home: home(env),
    };
    let no_system = env("GIT_CONFIG_NOSYSTEM").is_some_and(|v| {
        let v = v.to_string_lossy().to_ascii_lowercase();
        matches!(v.as_str(), "true" | "yes" | "on") || v.parse::<i64>().is_ok_and(|n| n != 0)
    });
    let mut files = Vec::new();
    // a variable set to nothing names no file, and no other is read
    // in its place
    if !no_system {
        files.push(match env("GIT_CONFIG_SYSTEM") {
            Some(named) => PathBuf::from(named),
            None => PathBuf::from("/etc/gitconfig"),
        });
    }
    match env("GIT_CONFIG_GLOBAL") {
        Some(only) => files.push(PathBuf::from(only)),
        None => {
            files.extend(user_dir(env).map(|dir| dir.join("config")));
            files.extend(home(env).map(|h| h.join(".gitconfig")));
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

/// The parts of a repository whose owner is looked at: its working
/// tree, where it has one, and its control directory. The first is the
/// directory `safe.directory` names.
pub fn looked_at(repo: &Repository) -> (&Path, Vec<&Path>) {
    let control = repo.git_dir();
    match repo.work_tree() {
        Some(tree) => (tree, vec![tree, control]),
        None => (control, vec![control]),
    }
}

/// Whether each of `paths`, the working tree `tree` of a repository and
/// its control directory, belongs to the user the process runs as, or
/// the configuration names `tree` safe.
pub fn owned_or_safe(env: Env, tree: &Path, paths: &[&Path]) -> Result<bool, RepoError> {
    let me = alt_os::sys::geteuid();
    // run through sudo, root may open what the user who called it owns
    let through = std::env::var("SUDO_UID").ok().filter(|_| me == 0);
    let through = through.and_then(|uid| uid.parse::<u32>().ok());
    let mut mine = true;
    for path in paths {
        let owner = path.symlink_metadata()?.uid();
        mine &= owner == me || Some(owner) == through;
    }
    Ok(mine || listed_safe(env, tree)?)
}

/// [`owned_or_safe`] of `repo` for a server, which git wants the
/// control directory named for: either directory named will do.
pub fn served_safe(env: Env, repo: &Repository) -> Result<bool, RepoError> {
    let (tree, paths) = looked_at(repo);
    Ok(owned_or_safe(env, tree, &paths)? || listed_safe(env, repo.git_dir())?)
}

/// What git says of a repository at `tree` that is another user's.
pub fn dubious(tree: &Path) -> String {
    let at = tree.display();
    format!(
        "fatal: detected dubious ownership in repository at '{at}'\n\
         To add an exception for this directory, call:\n\n\
         \tgit config --global --add safe.directory {at}\n"
    )
}

/// Whether `safe.directory`, in the configuration of the system and of
/// the user, names `tree`. `*` names every directory, and an empty value
/// takes back what was named before it.
fn listed_safe(env: Env, tree: &Path) -> Result<bool, RepoError> {
    let tree = tree.canonicalize()?;
    let mut safe = false;
    for entry in outer_listed(env, None, None)?.0 {
        if entry.section != "safe" || entry.subsection.is_some() || entry.key != "directory" {
            continue;
        }
        let value = entry.value.as_ref().map(|v| v.as_slice());
        safe = match value.unwrap_or_default() {
            b"" => false,
            b"*" => true,
            value => safe || names(env, value, &tree),
        };
    }
    Ok(safe)
}

/// Whether `named`, a value of `safe.directory`, is the directory
/// `tree`, as git reads the value: `~` is the home of the user, `.` the
/// directory the command runs in, and no other path that does not begin
/// at the root names anything. One that ends in `/*` names every
/// directory beneath it. The path is taken as the file system resolves
/// it, as `tree` is.
fn names(env: Env, named: &[u8], tree: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let named = Path::new(std::ffi::OsStr::from_bytes(named));
    let named = match (named.strip_prefix("~"), home(env)) {
        (Ok(below), Some(home)) => home.join(below),
        _ => named.to_owned(),
    };
    if named == Path::new(".") {
        let here = std::env::current_dir().and_then(|here| here.canonicalize());
        return here.is_ok_and(|here| here == tree);
    }
    if !named.is_absolute() {
        return false;
    }
    let resolved = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
    match named.file_name().is_some_and(|last| last == "*") {
        true => named
            .parent()
            .map(resolved)
            .is_some_and(|above| tree != above && tree.starts_with(above)),
        false => resolved(&named) == tree,
    }
}
