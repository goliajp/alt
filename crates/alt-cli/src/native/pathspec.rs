//! The paths a command names, as git reads them: relative to the directory
//! the command was run in, not to the root of the working tree.

use std::path::{Path, PathBuf};

use alt_repo::Repository;

use super::Res;

mod spec;
mod wild;

pub use spec::{Spec, keep_named, named};

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
/// does an absolute path that leads into the tree at `root`.
pub fn from_root(root: &Path, prefix: &str, given: &str) -> Res<Option<String>> {
    let top = given
        .strip_prefix(":/")
        .or_else(|| given.strip_prefix(":(top)"));
    match (top, given.starts_with('/')) {
        (Some(rest), _) => Ok(plain("", rest)),
        (None, true) => absolute(root, given),
        (None, false) => Ok(plain(prefix, given)),
    }
}

/// `rest` as it is named from `base`, its `.` and `..` taken out; `None`
/// where a `..` leads above where the names of `base` begin.
fn plain(base: &str, rest: &str) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    for name in base.split('/').chain(rest.split('/')) {
        match name {
            "" | "." => {}
            ".." => drop(names.pop()?),
            name => names.push(name),
        }
    }
    Some(names.join("/"))
}

/// What of the absolute path `given` lies in the tree at `root`, its `.`
/// and `..` taken out first: what follows the root as it is written, or
/// else what follows the shortest part of the path that is the root once
/// its links are followed. A link further in is a path of the tree, as
/// for git, which follows the links of one part after another and ends
/// where a name is missing that is not the last of its part.
fn absolute(root: &Path, given: &str) -> Res<Option<String>> {
    let Some(given) = plain("", given) else {
        return Ok(None);
    };
    let real = root.canonicalize().ok();
    let names: Vec<&str> = given.split('/').filter(|name| !name.is_empty()).collect();
    let upto = |n: usize| PathBuf::from(format!("/{}", names[..n].join("/")));
    let written = |n: &usize| upto(*n) == root || Some(upto(*n)) == real;
    let inside = |n: usize| Ok(Some(names[n..].join("/")));
    if let Some(n) = (0..=names.len()).find(written) {
        return inside(n);
    }
    let Some(real) = real else {
        return Ok(None);
    };
    let mut followed = PathBuf::from("/");
    for n in 1..=names.len() {
        use std::io::ErrorKind::NotFound;
        match upto(n).canonicalize() {
            Ok(part) if part == real => return inside(n),
            Ok(part) => followed = part,
            Err(e) if e.kind() == NotFound && n == names.len() => {}
            Err(e) => {
                let at = followed.join(names[n - 1]);
                let why = super::settings::reason(&e);
                let said = format!("Invalid path '{}': {why}", at.display());
                return Err(super::settings::fatal(said));
            }
        }
    }
    Ok(None)
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

    /// `blame` of the file `given` names as it is written, from `rev`.
    pub fn blame(
        &self,
        out: &mut impl std::io::Write,
        repo: &Repository,
        given: &str,
        rev: Option<&str>,
        options: crate::blame::Options,
    ) -> Res<()> {
        // git sets up its ignore rules for a blame, whatever it is of,
        // once it has found the revision and the path in it
        let control = match repo.is_native() {
            true => repo.git_dir().join("git-import"),
            false => repo.git_dir().to_owned(),
        };
        let info = control.join("info/exclude");
        let rev = rev.unwrap_or("HEAD");
        let path = self.written(given)?;
        let at = self.at_head(rev);
        let rules = super::settings::ensure_exclude_files(repo.config(), &self.root, &info);
        if let Err(ended) = rules {
            let known = |start| crate::tree_changes::blob_at(repo, start, &path).is_ok();
            if repo.rev_parse(&at)?.is_some_and(known) {
                return Err(ended);
            }
        }
        crate::blame::run(out, repo, &path, &at, rev, options)
    }

    /// The path from the root that `given` names, read without magic; a
    /// `/` at its end stays.
    fn written(&self, given: &str) -> Res<String> {
        let path = match given.starts_with('/') {
            true => absolute(&self.root, given)?,
            false => plain(&self.prefix, given),
        };
        let path = path.ok_or_else(|| self.outside(given))?;
        Ok(match given.ends_with('/') && !path.is_empty() {
            true => path + "/",
            false => path,
        })
    }

    /// The path from the root that a command run here names by `given`.
    pub fn path(&self, given: &str) -> Res<String> {
        from_root(&self.root, &self.prefix, given)?.ok_or_else(|| self.outside(given))
    }

    /// The file `rev:path` names. Only a path that starts with `./` or
    /// `../` is read from where the command runs, as for git.
    pub fn blob(&self, repo: &Repository, rev: &str, path: &str) -> Res<Vec<u8>> {
        let commit = repo
            .rev_parse(rev)?
            .ok_or_else(|| format!("bad revision '{rev}'"))?;
        let path = match path.starts_with("./") || path.starts_with("../") {
            true => self.path(path)?,
            false => path.to_owned(),
        };
        crate::tree_changes::blob_at(repo, commit, &path)
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
        let read = |prefix, given| from_root(root, prefix, given).unwrap();
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
        assert_eq!(read("sub", "/r/../r/x").as_deref(), Some("x"));
        assert_eq!(read("sub", "/x/../r/sub/../y").as_deref(), Some("y"));
        assert_eq!(read("sub", "/../r"), None);
    }

    #[test]
    fn an_absolute_path_through_a_link_to_the_tree_is_a_path_of_the_tree() {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("tree");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::os::unix::fs::symlink(&root, dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink("sub", root.join("inner")).unwrap();
        let link = dir.path().join("link");
        let read = |root: &Path, given: String| from_root(root, "sub", &given).unwrap();
        let through = format!("{}/sub/f", link.display());
        assert_eq!(read(&root, through.clone()).as_deref(), Some("sub/f"));
        // a tree that is itself named through the link
        assert_eq!(read(&link, through).as_deref(), Some("sub/f"));
        let real = format!("{}/sub/f", root.display());
        assert_eq!(read(&link, real).as_deref(), Some("sub/f"));
        // a link inside the tree is not followed
        let inner = format!("{}/inner/f", root.display());
        assert_eq!(read(&root, inner).as_deref(), Some("inner/f"));
        let beside = format!("{}/other", dir.path().display());
        assert_eq!(read(&root, beside), None);
        // a name missing in the tree is a path of the tree all the same
        let missing = format!("{}/nosuch/f", link.display());
        assert_eq!(read(&root, missing).as_deref(), Some("nosuch/f"));
        // one missing on the way there is where git ends
        let astray = format!("{}/nosuch/tree/f", dir.path().display());
        let ended = from_root(&root, "sub", &astray).unwrap_err().to_string();
        let at = dir.path().canonicalize().unwrap().join("nosuch");
        assert_eq!(
            ended,
            format!("Invalid path '{}': No such file or directory", at.display())
        );
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
