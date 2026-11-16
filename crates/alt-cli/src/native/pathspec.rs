//! The paths a command names, as git reads them: relative to the directory
//! the command was run in, not to the root of the working tree.

use std::path::{Path, PathBuf};

use alt_repo::Repository;

use super::Res;

/// Where `cwd` lies beneath the working tree at `root`: `sub/dir`, empty
/// for the root itself and for a directory outside the tree.
pub fn prefix_of(root: &Path, cwd: &Path) -> String {
    let beneath = cwd.strip_prefix(root).unwrap_or(Path::new(""));
    let names: Vec<_> = beneath.iter().map(|name| name.to_string_lossy()).collect();
    names.join("/")
}

/// The path from the root of the tree that `given` names from `prefix`,
/// without `.` or `..` in it and empty for the root itself; `None` when it
/// leaves the tree. `:/` and `:(top)` in front start at the root, and so
/// does an absolute path that lies beneath `root`.
pub fn from_root(root: &Path, prefix: &str, given: &str) -> Option<String> {
    let top = given
        .strip_prefix(":/")
        .or_else(|| given.strip_prefix(":(top)"));
    let (base, rest) = match (top, given.strip_prefix('/')) {
        (Some(rest), _) => ("", rest),
        (None, Some(_)) => ("", absolute(root, given)?),
        (None, None) => (prefix, given),
    };
    let mut names: Vec<&str> = base.split('/').filter(|name| !name.is_empty()).collect();
    for name in rest.split('/') {
        match name {
            "" | "." => {}
            ".." => drop(names.pop()?),
            name => names.push(name),
        }
    }
    Some(names.join("/"))
}

/// What of the absolute path `given` follows `root`. Names of `..` are
/// left for the caller: those that leave the tree leave it there as well.
fn absolute<'a>(root: &Path, given: &'a str) -> Option<&'a str> {
    let rest = given.strip_prefix(root.to_str()?)?;
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

/// `path`, which is relative to the root, as it is named from `prefix`.
pub fn shown_from(prefix: &str, path: &[u8]) -> Vec<u8> {
    if prefix.is_empty() {
        return path.to_vec();
    }
    let from: Vec<&[u8]> = prefix.as_bytes().split(|b| *b == b'/').collect();
    let to: Vec<&[u8]> = path.split(|b| *b == b'/').collect();
    // the last name of a path is never a directory both are in
    let reach = to.len().saturating_sub(1).min(from.len());
    let shared = (0..reach).take_while(|at| from[*at] == to[*at]).count();
    let mut shown = b"../".repeat(from.len() - shared);
    shown.extend_from_slice(&to[shared..].join(&b'/'));
    if shown.is_empty() {
        shown.extend_from_slice(b"./");
    }
    shown
}

/// What a command of the git layer goes by besides the repository: the
/// ref `HEAD` stands for, and where in which tree it was run.
pub struct Place {
    pub head: String,
    pub root: PathBuf,
    pub prefix: String,
}

impl Place {
    /// For a command run in `cwd`: the workspace's own HEAD and tree inside
    /// an alt repository, the bare `HEAD` in a plain git one. An explicitly
    /// named workspace that does not resolve is an error.
    pub fn of(repo: &Repository, cwd: &Path, workspace: Option<&str>) -> Res<Place> {
        match super::resolve_workspace(cwd, workspace) {
            Ok((_, coord)) => Ok(Place {
                head: coord.head_ref,
                root: coord.root,
                prefix: coord.prefix,
            }),
            Err(e) if workspace.is_some() => Err(e),
            Err(_) => {
                let root = repo.work_tree().unwrap_or(cwd).to_owned();
                let prefix = prefix_of(&root, &cwd.canonicalize()?);
                let head = "HEAD".to_owned();
                Ok(Place { head, root, prefix })
            }
        }
    }

    /// `HEAD` in a revision (also `HEAD~n`, `HEAD^`, `HEAD:path`) means the
    /// current workspace's HEAD, which outside the default workspace lives in
    /// `workspaces/<name>/HEAD` rather than the repository's bare `HEAD`.
    pub fn at_head(&self, spec: &str) -> String {
        match spec.strip_prefix("HEAD") {
            Some(rest) if rest.is_empty() || rest.starts_with(['~', '^', ':']) => {
                format!("{}{rest}", self.head)
            }
            _ => spec.to_owned(),
        }
    }

    /// The path from the root that a command run here names by `given`.
    pub fn path(&self, given: &str) -> Res<String> {
        from_root(&self.root, &self.prefix, given).ok_or_else(|| self.outside(given))
    }

    fn outside(&self, given: &str) -> Box<dyn std::error::Error> {
        let root = self.root.display();
        super::settings::fatal(format!("'{given}' is outside repository at '{root}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_read_from_where_the_command_runs() {
        let root = Path::new("/r");
        let read = |prefix, given| from_root(root, prefix, given);
        assert_eq!(read("", "a/b").as_deref(), Some("a/b"));
        assert_eq!(read("", "./a/").as_deref(), Some("a"));
        assert_eq!(read("sub", "f").as_deref(), Some("sub/f"));
        assert_eq!(read("sub", ".").as_deref(), Some("sub"));
        assert_eq!(read("sub", "..").as_deref(), Some(""));
        assert_eq!(read("sub/deep", "../../top").as_deref(), Some("top"));
        assert_eq!(read("sub", "deep/../f").as_deref(), Some("sub/f"));
        assert_eq!(read("sub", ":/").as_deref(), Some(""));
        assert_eq!(read("sub", ":(top)other").as_deref(), Some("other"));
        assert_eq!(read("sub", "/r/sub/../other").as_deref(), Some("other"));
        assert_eq!(read("sub", "/r").as_deref(), Some(""));
        assert_eq!(read("sub", "../.."), None);
        assert_eq!(read("sub", "/rest"), None);
        assert_eq!(read("sub", "/r/../x"), None);
        assert_eq!(read("", ".."), None);
    }

    #[test]
    fn a_path_is_shown_from_where_the_command_runs() {
        let shown = |prefix, path: &str| String::from_utf8(shown_from(prefix, path.as_bytes()));
        assert_eq!(shown("", "a/b").unwrap(), "a/b");
        assert_eq!(shown("sub", "sub/f").unwrap(), "f");
        assert_eq!(shown("sub", "top").unwrap(), "../top");
        assert_eq!(shown("sub/deep", "sub/f").unwrap(), "../f");
        assert_eq!(shown("sub/deep", "other/").unwrap(), "../../other/");
        assert_eq!(shown("sub", "sub/deep/").unwrap(), "deep/");
        assert_eq!(shown("sub", "sub/").unwrap(), "./");
        assert_eq!(shown("sub", "subway/f").unwrap(), "../subway/f");
        assert_eq!(shown("sub", "sub").unwrap(), "../sub");
    }
}
