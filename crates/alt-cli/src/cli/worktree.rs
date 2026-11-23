//! Running a command that puts files or index entries back, takes them
//! out or moves them.

use std::io::Write;

use super::{Command, Res};
use crate::native::{NativeRepo, Restore};
use crate::worktree_args;

pub(super) fn run<W: Write>(repo: &mut NativeRepo, cmd: &Command, out: &mut W) -> Res<u8> {
    match cmd {
        Command::Restore(args) => restore(repo, args, out)?,
        Command::Checkout(args) => checkout(repo, args, out)?,
        _ => return Err("not a native command".into()),
    }
    Ok(0)
}

/// The stage `--ours` or `--theirs` names.
fn side(ours: bool, theirs: bool) -> Option<u8> {
    match (ours, theirs) {
        (true, _) => Some(2),
        (false, true) => Some(3),
        (false, false) => None,
    }
}

fn fatal(said: &str) -> Box<dyn std::error::Error> {
    crate::git_settings::fatal(said.to_owned())
}

/// `{schema_version, paths}` for a command that says nothing otherwise.
fn report<W: Write>(paths: &[alt_bytes::BString], json: bool, out: &mut W) -> Res<()> {
    if json {
        let paths = paths.iter().map(|path| path.as_slice());
        crate::json::emit(out, crate::json::paths("paths", "paths_bytes", paths))?;
    }
    Ok(())
}

fn restore<W: Write>(repo: &mut NativeRepo, args: &worktree_args::Restore, out: &mut W) -> Res<()> {
    let side = side(args.ours, args.theirs);
    if args.paths.is_empty() {
        return Err(fatal("you must specify path(s) to restore"));
    }
    if side.is_some() && args.staged {
        return Err(fatal("'--ours' or '--theirs' cannot be used with --staged"));
    }
    if side.is_some() && args.source.is_some() {
        return Err(fatal("'--ours' or '--theirs' cannot be used with --source"));
    }
    let how = Restore {
        staged: args.staged,
        worktree: args.worktree || !args.staged,
        source: args.source.as_deref(),
        side,
        overlay: false,
    };
    let touched = repo.restore(&args.paths, &how)?;
    report(&touched, args.json, out)
}

/// `alt checkout`: a branch switched to where one name is given that is a
/// branch, else the path form, which restores the paths from the index, or
/// index and files from the revision named before them.
fn checkout<W: Write>(
    repo: &mut NativeRepo,
    args: &worktree_args::Checkout,
    out: &mut W,
) -> Res<()> {
    let side = side(args.ours, args.theirs);
    let names = &args.names;
    if side.is_some() && names.is_empty() {
        return Err(fatal("'--ours/--theirs' needs the paths to check out"));
    }
    if args.create {
        let [name] = &names[..] else {
            return Err(crate::machine::Failure::usage("-b takes one branch name").into());
        };
        return repo.switch(name, true, args.json, out);
    }
    if let ([name], None) = (&names[..], side)
        && repo.store_refs_get(&format!("refs/heads/{name}")).is_some()
    {
        return repo.switch(name, false, args.json, out);
    }
    if names.is_empty() {
        return Err(fatal("you must specify a branch or path(s) to check out"));
    }
    let source = match side {
        None if names.len() > 1 && repo.names_commit(&names[0])? => Some(names[0].as_str()),
        _ => None,
    };
    let how = Restore {
        staged: source.is_some(),
        worktree: true,
        source,
        side,
        overlay: true,
    };
    let paths = &names[usize::from(source.is_some())..];
    let touched = repo.restore(paths, &how)?;
    report(&touched, args.json, out)
}
