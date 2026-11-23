//! Running a native command.

use std::io::Write;

use super::prepare::prepare;
use super::*;
use crate::native::NativeRepo;

/// Runs a native command against an attached repo, returning the process exit
/// code (1 when a merge stops in conflict, else 0).
pub fn run_native<W: Write>(repo: &mut NativeRepo, cmd: &Command, out: &mut W) -> Res<u8> {
    let done = prepare(repo, cmd).and_then(|given_up| match given_up {
        Some(said) => Ok(writeln!(out, "{said}").map(|()| 0)?),
        None => dispatch(repo, cmd, out),
    });
    // what a command did before it failed is its step all the same, but
    // for a checkout it failed in the middle of
    let ended = repo.end_step();
    let code = done?;
    ended?;
    Ok(code)
}

fn dispatch<W: Write>(repo: &mut NativeRepo, cmd: &Command, out: &mut W) -> Res<u8> {
    if matches!(
        cmd,
        Command::Rebase {
            interactive: true,
            json: true,
            ..
        }
    ) {
        return Err(crate::machine::Failure::refusal(
            "interactive_input_required",
            "interactive rebase requires an editor and is unavailable in JSON mode",
        )
        .into());
    }
    match cmd {
        Command::Task { op, json } => repo.task_command(op, *json, out)?,
        Command::Candidate { op, json } => repo.candidate_command(op, *json, out)?,
        Command::Evidence { op, json } => repo.evidence_command(op, *json, out)?,
        Command::Request { op, json } => repo.request_query(op, *json, out)?,
        Command::Add { args } => repo.add_paths(args, out)?,
        Command::Commit {
            validate: true,
            json,
            ..
        } => {
            // exit 1 when a check fails, like a refused commit
            return Ok(if repo.validate(*json, out)? { 1 } else { 0 });
        }
        Command::Commit {
            message,
            amend,
            allow_empty,
            no_verify,
            decisions,
            extra,
            json,
            ..
        } => {
            let opts = crate::native::commit::CommitOptions {
                allow_empty: *allow_empty,
                no_verify: *no_verify,
                decisions: decisions.clone(),
                extra: extra.clone(),
            };
            if *amend {
                repo.amend(message.as_deref(), opts, *json, out)?
            } else {
                repo.commit(message.as_deref().unwrap_or_default(), opts, *json, out)?
            }
        }
        Command::Complete(args) => repo.complete(args, out)?,
        Command::Prompt(args) => repo.prompt(args, out)?,
        Command::Status { args } => repo.status(args.untracked(), args.json, out)?,
        Command::Branch(_) => return super::worktree::run(repo, cmd, out),
        Command::Tag(tag) => repo.tag(
            tag.name.clone(),
            tag.rev.clone(),
            tag.delete.clone(),
            tag.annotate,
            tag.message.clone(),
            tag.json,
            out,
        )?,
        Command::Diff(args) => repo.diff(&args.request(), out)?,
        Command::Meta { op } => match op {
            MetaOp::Show { rev, json } => repo.meta_show(rev, *json, out)?,
            MetaOp::Set {
                rev,
                message,
                decisions,
                extra,
            } => repo.meta_set(rev, message.as_deref(), decisions, extra, out)?,
        },
        Command::Lfs { op } => match op {
            LfsOp::Fetch { remote } => repo.lfs_fetch(remote, out)?,
            LfsOp::Import { git_dir } => repo.lfs_import(git_dir, out)?,
            LfsOp::Ls => repo.lfs_ls(out)?,
        },
        Command::Bisect { .. }
        | Command::Rebase { .. }
        | Command::Revert { .. }
        | Command::CherryPick { .. }
        | Command::Merge { .. } => return run_sequence(repo, cmd, out),
        Command::Flow { op, json } => match op {
            FlowOp::Init => repo.flow_init(*json, out)?,
            FlowOp::Feature {
                op: FlowTopicOp::Start { name },
            } => repo.flow_feature_start(name, *json, out)?,
            FlowOp::Feature {
                op: FlowTopicOp::Finish { name },
            } => repo.flow_feature_finish(name, *json, out)?,
            FlowOp::Release {
                op: FlowTopicOp::Start { name },
            } => repo.flow_release_start(name, *json, out)?,
            FlowOp::Release {
                op: FlowTopicOp::Finish { name },
            } => repo.flow_release_finish(name, *json, out)?,
            FlowOp::Hotfix {
                op: FlowTopicOp::Start { name },
            } => repo.flow_hotfix_start(name, *json, out)?,
            FlowOp::Hotfix {
                op: FlowTopicOp::Finish { name },
            } => repo.flow_hotfix_finish(name, *json, out)?,
        },
        Command::Undo { json } => repo.undo(*json, out)?,
        Command::Operation { op, json } => match op {
            OperationOp::Pending => repo.operation_pending(*json, out)?,
            OperationOp::Status { id } => repo.operation_status(id, *json, out)?,
            OperationOp::Recover { id } => repo.operation_recover(id, *json, out)?,
            OperationOp::Abandon { id, keep_changes } => {
                repo.operation_abandon(id, *keep_changes, *json, out)?
            }
        },
        Command::Workspace { op, json } => match op {
            WorkspaceOp::Add { name, path, branch } => {
                repo.workspace_add(name, path, branch.as_deref(), *json, out)?
            }
            WorkspaceOp::List => repo.workspace_list(*json, out)?,
            WorkspaceOp::Remove { name } => repo.workspace_remove(name, *json, out)?,
        },
        Command::Remote { op, json } => match op {
            RemoteOp::Add { name, url } => repo.remote_add(name, url, *json, out)?,
            RemoteOp::List => repo.remote_list(*json, out)?,
            RemoteOp::Remove { name } => repo.remote_remove(name, *json, out)?,
        },
        Command::Fetch {
            remote,
            refspecs,
            json,
        } => repo.fetch(remote, refspecs, *json, out)?,
        Command::Push {
            remote,
            refspecs,
            force,
            json,
        } => repo.push(remote, refspecs, *force, *json, out)?,
        Command::Identity { op, json } => match op {
            IdentityOp::Init { principal } => {
                repo.identity_init(principal.as_deref(), *json, out)?
            }
            IdentityOp::List => repo.identity_list(*json, out)?,
            IdentityOp::Trust {
                principal,
                pub_file,
            } => repo.identity_trust(principal, pub_file, *json, out)?,
        },
        Command::OpLog {
            max_count,
            json,
            verify,
        } => repo.op_log(*max_count, *json, *verify, out)?,
        Command::Verify {
            commits,
            max_count,
            json,
        } => repo.verify_commits(commits, *max_count, *json, out)?,
        _ => return worktree::run(repo, cmd, out),
    }
    Ok(0)
}

/// Runs a command that goes through commits one by one and may stop in
/// conflict, which its exit code tells.
fn run_sequence<W: Write>(repo: &mut NativeRepo, cmd: &Command, out: &mut W) -> Res<u8> {
    match cmd {
        Command::Bisect { op } => {
            match op {
                BisectOp::Start { bad, good } => repo.bisect_start(bad.as_deref(), good, out)?,
                BisectOp::Bad { revs } => repo.bisect_mark("bad", revs, out)?,
                BisectOp::Good { revs } => repo.bisect_mark("good", revs, out)?,
                BisectOp::Skip { revs } => repo.bisect_mark("skip", revs, out)?,
                BisectOp::Reset => repo.bisect_reset(out)?,
                BisectOp::Run { cmd } => repo.bisect_run(cmd, out)?,
            }
            Ok(0)
        }
        Command::Rebase {
            upstream,
            interactive,
            cont,
            skip,
            abort,
            json,
        } => {
            if *abort {
                repo.rebase_abort(out)?;
                return Ok(0);
            }
            let stopped = if *cont {
                repo.rebase_continue(*json, out)?
            } else if *skip {
                repo.rebase_skip(*json, out)?
            } else {
                let upstream = upstream.as_deref().unwrap_or_default();
                repo.rebase(upstream, *interactive, *json, out)?
            };
            // git exits 1 when a rebase stops in conflict
            Ok(if stopped { 1 } else { 0 })
        }
        Command::Revert {
            revs,
            mainline,
            cont,
            abort,
            json,
            ..
        } => {
            if *abort {
                repo.revert_abort(out)?;
                return Ok(0);
            }
            let stopped = if *cont {
                repo.revert_continue(*json, out)?
            } else {
                repo.revert(revs, *mainline, *json, out)?
            };
            // git exits 1 when a revert stops in conflict
            Ok(if stopped { 1 } else { 0 })
        }
        Command::CherryPick { args } => sequence::cherry_pick(repo, args, out),
        Command::Merge { args } => sequence::merge(repo, args, out),
        _ => Err("not a native command".into()),
    }
}
