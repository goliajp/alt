//! The rule that a branch is checked out in at most one workspace, the
//! marker a working tree points back at its repository with, and what a
//! workspace may be called.

use std::collections::BTreeMap;

use super::super::*;

/// A branch is checked out in at most one workspace, as in git. Moving or
/// deleting a branch another workspace has checked out would leave that
/// workspace's index and working tree describing a commit its HEAD no longer
/// names, so a commit there would silently undo the move; checking it out a
/// second time sets up the same trap. `own_head` is the HEAD ref of the
/// workspace making the change; `refs` is the state the change applies to.
pub(in crate::native) fn one_workspace_per_branch(
    own_head: &str,
    refs: &BTreeMap<String, RefTarget>,
    changes: &[RefChange],
) -> Result<(), String> {
    for c in changes {
        // the branch this change moves (any workspace but ours holding it is
        // a conflict) or checks out (any workspace but this one)
        let (branch, holder_ok) = if c.name.starts_with("refs/heads/") {
            (c.name.as_str(), own_head)
        } else if is_workspace_head(&c.name) {
            match &c.new {
                Some(RefTarget::Symbolic(b)) => (b.as_str(), c.name.as_str()),
                _ => continue,
            }
        } else {
            continue;
        };
        for (head, target) in refs {
            if head == holder_ok || !is_workspace_head(head) {
                continue;
            }
            if !matches!(target, RefTarget::Symbolic(b) if b == branch) {
                continue;
            }
            // that workspace lets go of the branch in this same transaction
            if changes
                .iter()
                .any(|o| o.name == *head && o.new.as_ref() != Some(target))
            {
                continue;
            }
            let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
            return Err(format!(
                "branch '{short}' is checked out in workspace '{}'",
                workspace_of(head)
            ));
        }
    }
    Ok(())
}

fn is_workspace_head(name: &str) -> bool {
    name == "HEAD" || (name.starts_with("workspaces/") && name.ends_with("/HEAD"))
}

fn workspace_of(head: &str) -> &str {
    head.strip_prefix("workspaces/")
        .and_then(|n| n.strip_suffix("/HEAD"))
        .unwrap_or(DEFAULT_WORKSPACE)
}

/// Reads a working tree's `.alt` marker file: line 1 is the repo root (the
/// directory holding the real `.alt`), line 2 is the workspace name.
pub(in crate::native) fn parse_workspace_marker(path: &Path) -> Res<(PathBuf, String)> {
    let content = std::fs::read_to_string(path)?;
    let mut lines = content.lines();
    let repo_root = lines.next().ok_or("malformed .alt workspace marker")?;
    let name = lines.next().ok_or("malformed .alt workspace marker")?;
    Ok((PathBuf::from(repo_root), name.to_owned()))
}

/// A workspace name: a single path segment (it becomes part of the ref name
/// `workspaces/<name>/HEAD` and a directory), so no slashes, dots, control or
/// special chars, and not the reserved default name.
pub(in crate::native) fn check_workspace_name(name: &str) -> Res<()> {
    let bad = name.is_empty()
        || name == DEFAULT_WORKSPACE
        || name.starts_with('.')
        || name.contains('/')
        || name.contains(['\\', ' ', '~', '^', ':', '?', '*', '[', '.'])
        || name.bytes().any(|b| b < 0x20 || b == 0x7f);
    if bad {
        return Err(format!("'{name}' is not a valid workspace name").into());
    }
    Ok(())
}
