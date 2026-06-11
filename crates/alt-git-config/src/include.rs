use std::path::{Path, PathBuf};

use bstr::{BStr, ByteSlice};

use crate::wildmatch::wildmatch;
use crate::{ConfigError, Entry, parse};

/// git's include recursion cap (`MAX_INCLUDE_DEPTH`).
pub(crate) const MAX_INCLUDE_DEPTH: usize = 10;

/// Repository facts `includeIf` conditions are evaluated against.
#[derive(Debug, Clone, Default)]
pub struct IncludeContext {
    /// Absolute, symlink-resolved `$GIT_DIR` (for `gitdir:`/`gitdir/i:`).
    pub git_dir: Option<PathBuf>,
    /// Current branch short name (for `onbranch:`).
    pub branch: Option<bstr::BString>,
    /// Home directory for `~/` expansion in paths and patterns.
    pub home: Option<PathBuf>,
}

/// Loads `path`, splicing included files in place, recursively.
pub(crate) fn load_with_includes(
    path: &Path,
    ctx: &IncludeContext,
) -> Result<Vec<Entry>, ConfigError> {
    let mut out = Vec::new();
    load_into(path, ctx, 0, &mut out)?;
    Ok(out)
}

fn load_into(
    path: &Path,
    ctx: &IncludeContext,
    depth: usize,
    out: &mut Vec<Entry>,
) -> Result<(), ConfigError> {
    if depth > MAX_INCLUDE_DEPTH {
        return Err(ConfigError::IncludeDepth);
    }
    let base = path.parent().unwrap_or(Path::new("."));
    for entry in parse::parse_file(&std::fs::read(path)?)? {
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
            Some(expand_path(value.as_bstr(), base, ctx))
        } else {
            None
        };
        out.push(entry);
        if let Some(target) = target {
            // like git: a missing include target is silently skipped
            if target.is_file() {
                load_into(&target, ctx, depth + 1, out)?;
            }
        }
    }
    Ok(())
}

/// `~/x` → home, relative → relative to the including file's directory.
fn expand_path(value: &BStr, base: &Path, ctx: &IncludeContext) -> PathBuf {
    let s = PathBuf::from(value.to_os_str().expect("unix bytes").to_owned());
    if let (Ok(rest), Some(home)) = (s.strip_prefix("~"), &ctx.home) {
        return home.join(rest);
    }
    if s.is_absolute() { s } else { base.join(s) }
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
    // config file's directory; M1 reads repo-local config, where the only
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
