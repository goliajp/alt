//! What a command is preceded by.

use super::{Command, WorkspaceOp, mutates_command};
use crate::native::NativeRepo;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Whether `cmd` is a step from its first record on, whatever it comes
/// to: it makes several commits, or hands HEAD to a scratch ref.
fn several_records(cmd: &Command) -> bool {
    match cmd {
        Command::Revert { revs, .. } => revs.len() > 1,
        Command::Rebase { upstream, .. } => upstream.is_some(),
        _ => false,
    }
}

/// The state file of the operation `cmd` gives up, and what it says once
/// that is given up.
fn gives_up(cmd: &Command) -> Option<(&'static str, &'static str)> {
    match cmd {
        Command::Merge { args } if args.abort_merge => Some(("MERGE_HEAD", "Merge aborted.")),
        Command::CherryPick { args } if args.abort_pick => {
            Some(("CHERRY_PICK_HEAD", "Cherry-pick aborted."))
        }
        Command::Revert { abort: true, .. } => Some(("REVERT_STATE", "Revert aborted.")),
        Command::Rebase { abort: true, .. } => Some(("REBASE_STATE", "Rebase aborted.")),
        _ => None,
    }
}

/// What comes before any command: git sets up its ignore rules for some
/// and ends there where one of the files they are in cannot be one. An
/// `--abort` may be done with here, and then this is what it says: the
/// command that began the operation was cut short and could not be
/// brought to an end, and giving that command up gave the operation up.
pub(super) fn prepare(repo: &mut NativeRepo, cmd: &Command) -> Res<Option<&'static str>> {
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
        // one under way is ended or gone on with all the same
        Merge { args } => {
            if let Some(branch) = &args.branch {
                repo.ensure_exclude_files_to_merge(branch)?;
            }
        }
        CherryPick { args } => {
            if let Some(rev) = &args.rev {
                repo.ensure_exclude_files_to_pick(rev)?;
            }
        }
        // the tree to come is checked out by the rules that hold in it
        Workspace {
            op: WorkspaceOp::Add { path, .. },
            ..
        } => repo.ensure_exclude_files_of(path)?,
        _ => {}
    }
    if mutates_command(cmd) && !matches!(cmd, Command::Operation { .. }) {
        if let Some((state, said)) = gives_up(cmd)
            && repo.begin_abort(state)?
        {
            return Ok(Some(said));
        }
        repo.begin_workspace_write()?;
        repo.forget_stale_cherry_pick()?;
        let validate = matches!(cmd, Commit { validate: true, .. });
        if matches!(
            cmd,
            Merge { .. } | CherryPick { .. } | Revert { .. } | Rebase { .. } | Commit { .. }
        ) && !validate
        {
            repo.begin_step(several_records(cmd))?;
        }
    }
    Ok(None)
}
