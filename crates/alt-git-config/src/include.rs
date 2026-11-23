use std::path::{Path, PathBuf};

use alt_bytes::{BStr, ByteSlice};

use crate::wildmatch::wildmatch;
use crate::{ConfigError, Entry, Origin, parse};

/// git's include recursion cap (`MAX_INCLUDE_DEPTH`).
pub(crate) const MAX_INCLUDE_DEPTH: usize = 10;

/// Repository facts `includeIf` conditions are evaluated against.
#[derive(Debug, Clone, Default)]
pub struct IncludeContext {
    /// Absolute, symlink-resolved `$GIT_DIR` (for `gitdir:`/`gitdir/i:`).
    pub git_dir: Option<PathBuf>,
    /// Current branch short name (for `onbranch:`).
    pub branch: Option<alt_bytes::BString>,
    /// Home directory for `~/` expansion in paths and patterns.
    pub home: Option<PathBuf>,
}

/// The files of a git directory kept in another place, with the working
/// tree that takes the place of the one the git directory lay in. A file
/// under `kept` includes by a relative path what the file it is a copy
/// of included: another file of the git directory, kept as well, or a
/// file of the working tree or beyond it.
#[derive(Debug, Clone)]
pub struct Moved {
    pub kept: PathBuf,
    pub tree: PathBuf,
}

/// Where a relative path leads from a directory of a git directory.
#[derive(Debug, PartialEq, Eq)]
pub enum Place {
    /// to a file of the git directory, by its path in there
    Kept(PathBuf),
    /// out of it, by the path from the working tree
    Tree(PathBuf),
}

/// Where `value` leads from the directory `dir` of a git directory, `dir`
/// given from the git directory. The path is followed as it is written,
/// not through the links a directory on it may be.
pub fn place(dir: &Path, value: &Path) -> Place {
    use std::path::Component;
    // the git directory itself is the first step from the working tree
    let mut steps: Vec<Option<&std::ffi::OsStr>> = vec![None];
    let mut above = 0;
    for part in dir.components().chain(value.components()) {
        match part {
            Component::Normal(name) => steps.push(Some(name)),
            Component::ParentDir if steps.pop().is_none() => above += 1,
            _ => {}
        }
    }
    let rest = |from: usize| steps[from..].iter().flatten().collect::<PathBuf>();
    match steps.first() {
        Some(None) => Place::Kept(rest(1)),
        _ => {
            let mut path: PathBuf = std::iter::repeat_n("..", above).collect();
            path.push(rest(0));
            Place::Tree(path)
        }
    }
}

/// Loads `path`, splicing included files in place, recursively. Also
/// returns every file that was read or looked for and not found.
pub(crate) fn load_with_includes(
    path: &Path,
    ctx: &IncludeContext,
    moved: Option<&Moved>,
) -> Result<(Vec<Entry>, Vec<PathBuf>), ConfigError> {
    let mut out = Vec::new();
    let mut files = Vec::new();
    let ctx = (ctx, moved);
    load_into(path, ctx, 0, &mut out, &mut files)?;
    Ok((out, files))
}

fn load_into(
    path: &Path,
    (ctx, moved): (&IncludeContext, Option<&Moved>),
    depth: usize,
    out: &mut Vec<Entry>,
    files: &mut Vec<PathBuf>,
) -> Result<(), ConfigError> {
    if depth > MAX_INCLUDE_DEPTH {
        return Err(ConfigError::IncludeDepth);
    }
    files.push(path.to_path_buf());
    let base = path.parent().unwrap_or(Path::new("."));
    let located = |(line, _)| ConfigError::Line(line, path.to_path_buf());
    let file: std::sync::Arc<Path> = path.into();
    for (line, mut entry) in parse::parse_located(&std::fs::read(path)?).map_err(located)? {
        entry.origin = Some(Origin {
            file: file.clone(),
            line,
        });
        let include = match (
            entry.section.as_str(),
            &entry.subsection,
            entry.key.as_str(),
        ) {
            ("include", None, "path") => true,
            ("includeif", Some(cond), "path") => condition_holds(cond.as_bstr(), ctx),
            _ => false,
        };
        // the directive entry itself stays in the list (as in git's
        // `--list --includes` output — and it must survive for config
        // restoration); included content is spliced right after it
        let target = if include {
            let Some(value) = &entry.value else {
                return Err(ConfigError::Syntax("include.path without a value"));
            };
            Some(
                expand_path(value.as_bstr(), base, ctx, moved).ok_or_else(|| {
                    ConfigError::IncludePath(value.to_string(), path.to_path_buf())
                })?,
            )
        } else {
            None
        };
        out.push(entry);
        if let Some(target) = target {
            // like git: a missing include target is silently skipped
            match target.is_file() {
                true => load_into(&target, (ctx, moved), depth + 1, out, files)?,
                false => files.push(target),
            }
        }
    }
    Ok(())
}

/// `~/x` → home, relative → relative to the including file's directory,
/// or to the one the file was in before it was moved.
/// `None` for a path in the home directory when none is known.
fn expand_path(
    value: &BStr,
    base: &Path,
    ctx: &IncludeContext,
    moved: Option<&Moved>,
) -> Option<PathBuf> {
    let s = PathBuf::from(value.to_os_str().expect("unix bytes").to_owned());
    if let Ok(rest) = s.strip_prefix("~") {
        return ctx.home.as_ref().map(|home| home.join(rest));
    }
    if s.is_absolute() {
        return Some(s);
    }
    let was = moved.and_then(|moved| Some((moved, base.strip_prefix(&moved.kept).ok()?)));
    Some(match was {
        Some((moved, dir)) => match place(dir, &s) {
            Place::Kept(path) => moved.kept.join(path),
            Place::Tree(path) => moved.tree.join(path),
        },
        None => base.join(s),
    })
}

fn condition_holds(cond: &BStr, ctx: &IncludeContext) -> bool {
    if let Some(pat) = cond.strip_prefix(b"gitdir:") {
        return gitdir_matches(pat, ctx, false);
    }
    if let Some(pat) = cond.strip_prefix(b"gitdir/i:") {
        return gitdir_matches(pat, ctx, true);
    }
    if let Some(pat) = cond.strip_prefix(b"onbranch:") {
        let Some(branch) = &ctx.branch else {
            return false;
        };
        let mut pat = pat.to_vec();
        if pat.ends_with(b"/") {
            pat.extend_from_slice(b"**");
        }
        return wildmatch(&pat, branch, false);
    }
    // unknown conditions are false, never an error (forward compatibility,
    // matching git)
    false
}

fn gitdir_matches(pattern: &[u8], ctx: &IncludeContext, case_insensitive: bool) -> bool {
    let Some(git_dir) = &ctx.git_dir else {
        return false;
    };
    let mut pat = pattern.to_vec();
    if let Some(rest) = pat.strip_prefix(b"~/") {
        let Some(home) = &ctx.home else { return false };
        let mut p = home.as_os_str().as_encoded_bytes().to_vec();
        p.push(b'/');
        p.extend_from_slice(rest);
        pat = p;
    }
    // `./` is relative to the including file — git resolves it against the
    // config file's directory; only repo-local config is read, where the only
    // sensible anchor is the git dir's parent. Revisit with global config.
    if !(pat.starts_with(b"/") || pat.starts_with(b"**") || pat.starts_with(b"./")) {
        let mut p = b"**/".to_vec();
        p.extend_from_slice(&pat);
        pat = p;
    }
    if pat.ends_with(b"/") {
        pat.extend_from_slice(b"**");
    }
    let dir = git_dir.as_os_str().as_encoded_bytes();
    wildmatch(&pat, dir, case_insensitive)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(git_dir: &str, branch: &str) -> IncludeContext {
        IncludeContext {
            git_dir: Some(git_dir.into()),
            branch: Some(branch.into()),
            home: Some("/home/u".into()),
        }
    }

    #[test]
    fn a_relative_path_leads_where_it_led_from_the_git_directory() {
        let kept = |path: &str| Place::Kept(path.into());
        let tree = |path: &str| Place::Tree(path.into());
        let from = |dir: &str, value: &str| place(Path::new(dir), Path::new(value));
        assert_eq!(from("", "extra"), kept("extra"));
        assert_eq!(from("", "./sub/../extra"), kept("extra"));
        assert_eq!(from("sub", "../extra"), kept("extra"));
        assert_eq!(from("", "../.gitconfig"), tree(".gitconfig"));
        assert_eq!(from("sub", "../../a/b"), tree("a/b"));
        assert_eq!(from("", "../../out/x"), tree("../out/x"));
        assert_eq!(from("sub", "../../../../x"), tree("../../x"));
        // a way back in is not the git directory: its name is not known
        assert_eq!(from("", "../.git/extra"), tree(".git/extra"));
    }

    #[test]
    fn gitdir_conditions() {
        let c = ctx("/work/proj/.git", "main");
        assert!(condition_holds(b"gitdir:/work/proj/.git".into(), &c));
        assert!(condition_holds(b"gitdir:proj/.git".into(), &c)); // **/ prepended
        assert!(condition_holds(b"gitdir:/work/".into(), &c)); // ** appended
        assert!(condition_holds(b"gitdir:**/proj/**".into(), &c));
        assert!(!condition_holds(b"gitdir:/other/".into(), &c));
        assert!(!condition_holds(b"gitdir:/WORK/".into(), &c));
        assert!(condition_holds(b"gitdir/i:/WORK/".into(), &c));
    }

    #[test]
    fn onbranch_and_unknown_conditions() {
        let c = ctx("/x/.git", "feature/login");
        assert!(condition_holds(b"onbranch:feature/login".into(), &c));
        assert!(condition_holds(b"onbranch:feature/".into(), &c));
        assert!(condition_holds(b"onbranch:feature/*".into(), &c));
        assert!(!condition_holds(b"onbranch:main".into(), &c));
        assert!(!condition_holds(b"hasconfig:remote.*.url:x".into(), &c));
    }
}
