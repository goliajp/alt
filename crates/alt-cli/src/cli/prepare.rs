//! What a command is preceded by.

use super::{Command, WorkspaceOp, mutates_command};
use crate::native::NativeRepo;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// What comes before any command: git sets up its ignore rules for some
/// and ends there where one of the files they are in cannot be one.
pub(super) fn prepare(repo: &mut NativeRepo, cmd: &Command) -> Res<()> {
    use Command::{
        Add, CherryPick, Commit, Diff, Merge, Rebase, Revert, Status, Switch, Workspace,
    };
    let excludes = match cmd {
        Add { args } => !args.renormalize,
        Commit { validate, .. } => !validate,
        Diff { cached, .. } => *cached,
        Switch { create, .. } => !create,
        // a sequence under way is ended or gone on with all the same
        Rebase { upstream, .. } => upstream.is_some(),
        Revert { revs, .. } => !revs.is_empty(),
        _ => matches!(cmd, Status { .. }),
    };
    if excludes {
        repo.ensure_exclude_files()?;
    }
    match cmd {
        Merge { branch, .. } => repo.ensure_exclude_files_to_merge(branch)?,
        CherryPick { rev, .. } => repo.ensure_exclude_files_to_pick(rev)?,
        // the tree to come is checked out by the rules that hold in it
        Workspace {
            op: WorkspaceOp::Add { path, .. },
            ..
        } => repo.ensure_exclude_files_of(path)?,
        _ => {}
    }
    if mutates_command(cmd) && !matches!(cmd, Command::Operation { .. }) {
        repo.begin_workspace_write()?;
        repo.forget_stale_cherry_pick()?;
    }
    Ok(())
}
