//! Running a command that puts files or index entries back, takes them
//! out or moves them.

use std::io::Write;

use super::{Command, Res};
use crate::native::{NativeRepo, ResetMode, Restore, Said};
use crate::worktree_args;

pub(super) fn run<W: Write>(repo: &mut NativeRepo, cmd: &Command, out: &mut W) -> Res<u8> {
    match cmd {
        Command::Restore(args) => restore(repo, args, out)?,
        Command::Checkout(args) => checkout(repo, args, out)?,
        Command::Reset(args) => reset(repo, args, out)?,
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
    if side.is_some() && names.is_empty() && args.paths.is_empty() {
        return Err(fatal("'--ours/--theirs' needs the paths to check out"));
    }
    if args.create {
        let [name] = &names[..] else {
            return Err(crate::machine::Failure::usage("-b takes one branch name").into());
        };
        return repo.switch(name, true, args.json, out);
    }
    if let ([name], None, true) = (&names[..], side, args.paths.is_empty())
        && repo.store_refs_get(&format!("refs/heads/{name}")).is_some()
    {
        return repo.switch(name, false, args.json, out);
    }
    if names.is_empty() && args.paths.is_empty() {
        return Err(fatal("you must specify a branch or path(s) to check out"));
    }
    // after `--` there are only paths, and one name before it is a revision
    let (source, paths) = match (&names[..], side, args.paths.is_empty()) {
        ([], _, false) => (None, &args.paths[..]),
        ([rev], None, false) => (Some(rev.as_str()), &args.paths[..]),
        (_, _, false) => {
            let usage = "only one revision may come before `--`";
            return Err(crate::machine::Failure::usage(usage).into());
        }
        ([rev, paths @ ..], None, true) if !paths.is_empty() && repo.names_commit(rev)? => {
            (Some(rev.as_str()), paths)
        }
        (names, _, true) => (None, names),
    };
    let how = Restore {
        staged: source.is_some(),
        worktree: true,
        source,
        side,
        overlay: true,
    };
    let touched = repo.restore(paths, &how)?;
    report(&touched, args.json, out)
}

/// `alt reset`: to a commit where no path is given, else the index
/// entries of the paths. A name before `--` is the commit; without `--`
/// the first name is the commit where it names one.
fn reset<W: Write>(repo: &mut NativeRepo, args: &worktree_args::Reset, out: &mut W) -> Res<()> {
    let names = &args.names;
    let (rev, paths) = match (&names[..], args.paths.is_empty()) {
        ([], _) => (None, &args.paths[..]),
        ([rev], false) => (Some(rev.as_str()), &args.paths[..]),
        (_, false) => {
            let usage = "only one revision may come before `--`";
            return Err(crate::machine::Failure::usage(usage).into());
        }
        ([rev, paths @ ..], true) if repo.names_commit(rev)? => (Some(rev.as_str()), paths),
        ([first, ..], true) if repo.names_path(first)? => (None, &names[..]),
        // what is neither is said to be no revision
        ([rev, ..], true) => (Some(rev.as_str()), &names[..0]),
    };
    let mode = match (args.soft, args.hard) {
        (true, _) => ResetMode::Soft,
        (false, true) => ResetMode::Hard,
        (false, false) => ResetMode::Mixed,
    };
    let said = Said {
        quiet: args.quiet,
        json: args.json,
    };
    if paths.is_empty() {
        return repo.reset(rev, mode, said, out);
    }
    if mode != ResetMode::Mixed {
        let mode = if args.soft { "soft" } else { "hard" };
        return Err(fatal(&format!("Cannot do {mode} reset with paths.")));
    }
    repo.reset_paths(rev, paths, said, out)
}
