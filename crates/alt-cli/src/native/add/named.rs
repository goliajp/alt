//! What the paths given to `add` name: which of them name nothing, which
//! lead to something an ignore rule hides, and which cannot be named at
//! all.

use alt_worktree::Delta;

use super::*;
use crate::native::pathspec::Spec;

/// Whether `spec` names something inside the repository of its own that
/// the scan reported as the directory `path`, which ends in `/`. Such a
/// path matches and stages nothing, as for git.
fn inside_repository(path: &[u8], spec: &str) -> bool {
    path.ends_with(b"/") && spec.as_bytes().starts_with(path)
}

/// Whether `path` is `dir` or lies beneath it; the empty `dir` is the tree.
fn within(path: &[u8], dir: &[u8]) -> bool {
    let beneath = |rest: &[u8]| rest.is_empty() || rest[0] == b'/';
    dir.is_empty() || path.strip_prefix(dir).is_some_and(beneath)
}

/// The directory git walks to find what `specs` name for a command run in
/// `prefix`: the innermost that holds every path given as far as it is
/// written out, those that take away left aside.
fn walked<'a>(specs: &'a [Spec], prefix: &'a str) -> &'a str {
    let mut dirs = specs.iter().filter(|s| !s.exclude).map(Spec::walked);
    let Some(mut walked) = dirs.next() else {
        return prefix;
    };
    for dir in dirs {
        while !within(dir.as_bytes(), walked.as_bytes()) {
            walked = walked.rfind('/').map_or("", |slash| &walked[..slash]);
        }
    }
    walked
}

/// What a path given to `add` may name.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Known {
    /// any file
    Any,
    /// a file of the index; a file there is that the index has not is
    /// passed over
    Tracked,
    /// a file of the index, and a file the index has not is an error
    TrackedOrError,
}

/// Ends the command where git ends its walk of the tree: on a path given
/// from the top with a name of no length in the directories it leads
/// through.
pub(super) fn ensure_walkable(specs: &[Spec]) -> Res<()> {
    let empty_name =
        |dir: &str| !dir.starts_with('/') && (dir.ends_with('/') || dir.contains("//"));
    match specs
        .iter()
        .any(|spec| !spec.exclude && empty_name(spec.walked()))
    {
        true => Err(settings::fatal("oops in prep_exclude".into())),
        false => Ok(()),
    }
}

impl NativeRepo<'_> {
    /// A path stands for itself and every file under it, deleted ones
    /// included (their removal is staged), as in git; a conflicted path
    /// is one the index knows, with or without its file. A path that
    /// names nothing is an error, unless it is written out and there is
    /// something at it: what `known` then has for a file the index has
    /// not.
    pub(super) fn ensure_matched(
        &self,
        specs: &[Spec],
        before: &Index,
        delta: &Delta,
        known: Known,
    ) -> Res<()> {
        let (mut unknown, mut any_tracked) = (Vec::new(), false);
        let named = specs.iter().filter(|s| !s.exclude && !s.path.is_empty());
        for spec in named {
            let tracked = before.entries.iter().any(|e| spec.matches(&e.path));
            any_tracked |= tracked;
            let new = delta.untracked.iter().chain(&delta.unmerged);
            let seen = |p: &BString| spec.matches(p) || inside_repository(p, &spec.path);
            if tracked || (known == Known::Any && new.into_iter().any(seen)) {
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
            unknown.push(spec.given.as_str());
        }
        if unknown.is_empty() || known != Known::TrackedOrError {
            return Ok(());
        }
        // git tries a path that takes away only on a file another path
        // names, and tells of it as well when it tried it on none
        let told = |spec: &&Spec| match spec.exclude {
            true => !any_tracked,
            false => unknown.contains(&spec.given.as_str()),
        };
        let told: Vec<&str> = specs
            .iter()
            .filter(told)
            .map(|s| s.given.as_str())
            .collect();
        Err(not_known(&told)?.into())
    }

    /// The paths git tells of because an ignore rule hides them: each the
    /// outermost path hidden that a path given leads to or into as far as
    /// it is written out, one that takes away as well, and that git meets
    /// on its walk. A file of the index is hidden by no rule.
    pub(super) fn hidden(&self, specs: &[Spec], before: &Index) -> Res<Vec<BString>> {
        let walked = walked(specs, &self.prefix).as_bytes();
        let inputs = self.scan_inputs()?;
        let excludes: Vec<&Path> = inputs.excludes.iter().map(PathBuf::as_path).collect();
        let mut rules = alt_worktree::Hidden::new(&self.root, &excludes)?;
        let mut hidden = Vec::new();
        for spec in specs.iter().filter(|spec| !spec.literal().is_empty()) {
            let literal = spec.literal().as_bytes();
            let Some(by) = rules.by(literal)? else {
                continue;
            };
            let met = within(&by, walked) || within(walked, &by);
            let by = BString::from(by);
            let tracked = before.entries.binary_search_by(|e| e.path.cmp(&by)).is_ok();
            if met && !tracked && std::fs::symlink_metadata(self.abs(&by)?).is_ok() {
                hidden.push(by);
            }
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

/// git's error for each of the paths `given` that name no file of the
/// index, and the failure a command ends with that has said them.
fn not_known(given: &[&str]) -> Res<crate::machine::Failure> {
    for given in given {
        let said = format!("error: pathspec '{given}' did not match any file(s) known to git\n");
        crate::diagnostics::emit(said.as_bytes())?;
    }
    let said = "a path names no file of the index";
    let failure = crate::machine::Failure::refusal(crate::machine::PATHS_UNKNOWN, said);
    Ok(crate::machine::Failure {
        exit_code: 128,
        ..failure
    })
}

/// What `add` says when it is given no path, and how to add everything
/// unless `advice.addEmptyPathspec` is off.
pub(super) fn nothing_specified(settings: &alt_git_config::Config) -> Res<String> {
    let advise = match settings.get_bool("advice", None, "addemptypathspec") {
        Some(set) => set?,
        None => true,
    };
    let mut said = String::from("Nothing specified, nothing added.\n");
    if advise {
        said.push_str(EMPTY_ADVICE);
    }
    Ok(said)
}

const EMPTY_ADVICE: &str = "\
hint: Maybe you wanted to say 'alt add .'?
hint: Disable this message with \"alt config advice.addEmptyPathspec false\"
";

/// What `add` says of the `paths` it left alone because an ignore rule
/// hides them: git's list, each path as its bytes are, and how to add
/// them all the same unless `advice.addIgnoredFile` is off.
pub(super) fn ignored(paths: &[BString], settings: &alt_git_config::Config) -> Res<Vec<u8>> {
    let advise = match settings.get_bool("advice", None, "addignoredfile") {
        Some(set) => set?,
        None => true,
    };
    let mut said = b"The following paths are ignored by one of your .gitignore files:\n".to_vec();
    for path in paths {
        said.extend_from_slice(path);
        said.push(b'\n');
    }
    if advise {
        said.extend_from_slice(ADVICE.as_bytes());
    }
    Ok(said)
}

/// The failure an `add` ends with that left `paths` alone: they are named
/// in its details, each with its bytes beside it where it is not UTF-8.
pub(super) fn left_alone(paths: &[BString]) -> crate::machine::Failure {
    let named = |path: &BString| match std::str::from_utf8(path) {
        Ok(text) => alt_json::json!({"path": text}),
        Err(_) => alt_json::json!({
            "path": String::from_utf8_lossy(path),
            "path_bytes": crate::machine::hex(path),
        }),
    };
    let paths: Vec<alt_json::Value> = paths.iter().map(named).collect();
    let said = "paths that an ignore rule hides were not added";
    let mut failure = crate::machine::Failure::refusal(crate::machine::PATHS_IGNORED, said);
    *failure.details = alt_json::json!({"ignored_paths": paths});
    failure
}

/// How an `add` ends that left the paths `hidden` alone or `failed` to
/// store a path: with the status git has for it, 255 for the failure
/// whatever else there is.
pub(super) fn ended(hidden: &[BString], failed: bool) -> Res<()> {
    let said = "a path could not be added";
    let failure = crate::machine::Failure::refusal(crate::machine::PATHS_FAILED, said);
    match (failed, hidden.is_empty()) {
        (true, _) => Err(Box::new(crate::machine::Failure {
            exit_code: 255,
            ..failure
        })),
        (false, false) => Err(left_alone(hidden).into()),
        (false, true) => Ok(()),
    }
}

const ADVICE: &str = "\
hint: Use -f if you really want to add them.
hint: Disable this message with \"alt config advice.addIgnoredFile false\"
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_left_alone_that_is_not_utf8_has_its_bytes_in_the_details() {
        let failure = left_alone(&[BString::from("x.log"), BString::from(&b"h\xff"[..])]);
        let paths = alt_json::json!([
            {"path": "x.log"},
            {"path": "h\u{fffd}", "path_bytes": "68ff"},
        ]);
        assert_eq!(failure.details["ignored_paths"], paths);
        assert_eq!((failure.kind, failure.exit_code), ("paths_ignored", 1));
    }
}
