//! What the paths given to `add` name: which of them name nothing, which
//! only something an ignore rule hides, and which cannot be named at all.

use alt_worktree::Delta;

use super::*;
use crate::native::pathspec::Spec;

/// Whether `spec` names something inside the repository of its own that
/// the scan reported as the directory `path`, which ends in `/`. Such a
/// path matches and stages nothing, as for git.
fn inside_repository(path: &[u8], spec: &str) -> bool {
    path.ends_with(b"/") && spec.as_bytes().starts_with(path)
}

impl NativeRepo<'_> {
    /// A path stands for itself and every file under it, deleted ones
    /// included (their removal is staged), as in git; a conflicted path
    /// is one the index knows, with or without its file. A path that
    /// names nothing is an error, unless it is written out and there is
    /// something at it: returns those of them an ignore rule hides, each
    /// as the outermost directory hidden. With `tracked_only`, what is
    /// not in the index is not named and nothing is returned.
    pub(super) fn unmatched(
        &self,
        specs: &[Spec],
        before: &Index,
        delta: &Delta,
        tracked_only: bool,
    ) -> Res<Vec<BString>> {
        let mut hidden = Vec::new();
        let named = specs.iter().filter(|s| !s.exclude && !s.path.is_empty());
        for spec in named {
            let tracked = before.entries.iter().any(|e| spec.matches(&e.path));
            let new = delta.untracked.iter().chain(&delta.unmerged);
            let seen = |p: &BString| spec.matches(p) || inside_repository(p, &spec.path);
            if tracked || (!tracked_only && new.into_iter().any(seen)) {
                continue;
            }
            let place = match spec.dir {
                true => format!("{}/", spec.path),
                false => spec.path.clone(),
            };
            let there = std::fs::symlink_metadata(self.root.join(place)).is_ok();
            if !spec.written_out() || !there {
                let given = &spec.given;
                return Err(settings::fatal(format!(
                    "pathspec '{given}' did not match any files"
                )));
            }
            if tracked_only {
                continue;
            }
            let inputs = self.scan_inputs()?;
            let excludes: Vec<&Path> = inputs.excludes.iter().map(PathBuf::as_path).collect();
            let by = alt_worktree::hidden_by(&self.root, spec.path.as_bytes(), &excludes)?;
            hidden.extend(by.map(BString::from));
        }
        hidden.sort();
        hidden.dedup();
        Ok(hidden)
    }

    /// Refuses a path named through a symlink, which is not a path of this
    /// tree, and one inside a submodule, which is the submodule's to add.
    pub(super) fn ensure_reachable(&self, specs: &[Spec], before: &Index) -> Res<()> {
        for spec in specs {
            let given = &spec.given;
            // with `/` at the end, the path itself is a directory to go through
            let path = match spec.dir {
                true => format!("{}/", spec.path),
                false => spec.path.clone(),
            };
            let mut parents = path.match_indices('/').map(|(at, _)| &path[..at]);
            if parents.any(|p| self.root.join(p).is_symlink()) {
                return Err(settings::fatal(format!(
                    "pathspec '{given}' is beyond a symbolic link"
                )));
            }
            let inside = before.entries.iter().find(|e| {
                e.mode == 0o160000
                    && spec
                        .path
                        .as_bytes()
                        .strip_prefix(e.path.as_slice())
                        .is_some_and(|rest| rest.starts_with(b"/"))
            });
            if let Some(sub) = inside {
                let sub = &sub.path;
                return Err(settings::fatal(format!(
                    "Pathspec '{given}' is in submodule '{sub}'"
                )));
            }
        }
        Ok(())
    }
}

/// What `add` says of the `paths` it left alone because an ignore rule
/// hides them: git's list, and how to add them all the same unless
/// `advice.addIgnoredFile` is off.
pub(super) fn ignored(paths: &[BString], settings: &alt_git_config::Config) -> Res<String> {
    let advise = match settings.get_bool("advice", None, "addignoredfile") {
        Some(set) => set?,
        None => true,
    };
    let mut said =
        String::from("The following paths are ignored by one of your .gitignore files:\n");
    for path in paths {
        said.push_str(&format!("{path}\n"));
    }
    if advise {
        said.push_str(ADVICE);
    }
    Ok(said)
}

const ADVICE: &str = "\
hint: Use -f if you really want to add them.
hint: Disable this message with \"alt config advice.addIgnoredFile false\"
";
