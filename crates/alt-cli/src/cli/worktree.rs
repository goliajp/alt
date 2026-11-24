//! Running a command that puts files or index entries back, takes them
//! out or moves them.

use std::io::Write;

use super::{Command, Res};
use crate::native::{Create, Mv, NativeRepo, ResetMode, Restore, Rm, Said};
use crate::rev_or_path::Words;
use crate::worktree_args;

pub(super) fn run<W: Write>(repo: &mut NativeRepo, cmd: &Command, out: &mut W) -> Res<u8> {
    match cmd {
        Command::Branch(args) if args.rename || args.rename_over => {
            let Some(name) = &args.name else {
                return Err(fatal("branch name required"));
            };
            let (old, new) = match &args.start {
                Some(new) => (Some(name.as_str()), new),
                None => (None, name),
            };
            repo.branch_rename(old, new, args.rename_over, out)?
        }
        Command::Branch(args) if args.force_move => {
            let Some(name) = &args.name else {
                return Err(fatal("branch name required"));
            };
            repo.branch_force(name, args.start.as_deref(), out)?
        }
        Command::Branch(args) => {
            let (name, start, delete) =
                (args.name.clone(), args.start.clone(), args.delete.clone());
            repo.branch(name, start, delete, args.remotes, args.json, out)?
        }
        Command::Switch(args) => {
            let start = args.start.as_deref();
            let (make, over) = (args.create || args.create_over, args.create_over);
            if start.is_some() && !make {
                return Err(fatal("only one reference expected"));
            }
            let create = make.then_some(Create { start, over });
            let left = match args.name == "-" && create.is_none() {
                true => Some(
                    repo.branch_left()?
                        .ok_or_else(|| invalid_reference("@{-1}"))?,
                ),
                false => None,
            };
            let name = left.as_deref().unwrap_or(&args.name);
            repo.switch_command(name, create, args.json, out)?
        }
        Command::Restore(args) => restore(repo, args, out)?,
        Command::Checkout(args) => checkout(repo, args, out)?,
        Command::Reset(args) => reset(repo, args, out)?,
        Command::Rm(args) => rm(repo, args, out)?,
        Command::Mv(args) => mv(repo, args, out)?,
        Command::Stash(args) => return super::stash::run(repo, args, out),
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

fn invalid_reference(name: &str) -> Box<dyn std::error::Error> {
    fatal(&format!("invalid reference: {name}"))
}

/// The revision the paths of a checkout are restored from, none for the
/// index, and the paths. After `--` there are only paths, and one name
/// before it is a revision; without it the first of several names is the
/// revision where it names one. `sided` is for `--ours` or `--theirs`.
fn checkout_source<'a>(
    repo: &NativeRepo,
    args: &'a worktree_args::Checkout,
    sided: bool,
) -> Res<(Option<&'a str>, &'a [String])> {
    Ok(match (&args.names[..], sided, args.dashed) {
        ([], _, true) => (None, &args.paths[..]),
        ([rev], false, true) if repo.names_commit(rev)? => (Some(rev.as_str()), &args.paths[..]),
        ([rev], false, true) => return Err(invalid_reference(rev)),
        (_, _, true) => {
            let usage = "only one revision may come before `--`";
            return Err(crate::machine::Failure::usage(usage).into());
        }
        ([rev, paths @ ..], false, false) if !paths.is_empty() && repo.names_commit(rev)? => {
            if repo.file_named(rev)? {
                return Err(crate::rev_or_path::both(rev));
            }
            (Some(rev.as_str()), paths)
        }
        (names, _, false) => (None, names),
    })
}

/// `alt checkout -b <name> [<start>]`: the branch made, at the commit
/// `start` names where one is given, and switched to.
fn checkout_new<W: Write>(
    repo: &mut NativeRepo,
    args: &worktree_args::Checkout,
    out: &mut W,
) -> Res<()> {
    let words: Vec<&String> = args.names.iter().chain(&args.paths).collect();
    let (name, start) = match words[..] {
        [] => return Err(crate::machine::Failure::usage("-b takes a branch name").into()),
        [name] => (name, None),
        [name, start] => (name, Some(start.as_str())),
        [name, ..] => {
            let said = "Cannot update paths and switch to branch";
            return Err(fatal(&format!("{said} '{name}' at the same time.")));
        }
    };
    if let Some(start) = start
        && !repo.names_commit(start)?
    {
        let whose = format!("a branch '{name}' cannot be created from it");
        return Err(fatal(&format!("'{start}' is not a commit and {whose}")));
    }
    let over = args.create_over;
    repo.switch(name, Some(Create { start, over }), args.json, out)
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
    if args.create || args.create_over {
        return checkout_new(repo, args, out);
    }
    if let ([name], None, true) = (&names[..], side, args.paths.is_empty()) {
        // `-` is the branch left last; with none it is a path, as in git
        if let Some(left) = repo.branch_left()?.filter(|_| name == "-") {
            return repo.switch(&left, None, args.json, out);
        }
        if repo.store_refs_get(&format!("refs/heads/{name}")).is_some() {
            return repo.switch(name, None, args.json, out);
        }
        // before `--` the name is no path: what is no branch is refused
        if args.dashed && !repo.names_commit(name)? {
            return Err(invalid_reference(name));
        }
        // a name that is a commit is never a path, though a file has it
        if args.dashed || repo.names_commit(name)? {
            return Err(crate::native::said::no_branch_at(name));
        }
        if let Some(tracking) = repo.tracked_by_one_remote(name)? {
            if repo.file_named(name)? {
                let said = format!(
                    "fatal: '{name}' could be both a local file and a tracking branch.\n\
                     Please use -- (and optionally --no-guess) to disambiguate\n"
                );
                return Err(crate::native::said::refused(&said, 128));
            }
            let start = Some(tracking.as_str());
            let create = Create { start, over: false };
            return repo.switch(name, Some(create), args.json, out);
        }
    }
    if names.is_empty() && args.paths.is_empty() {
        return Err(fatal("you must specify a branch or path(s) to check out"));
    }
    let (source, paths) = checkout_source(repo, args, side.is_some())?;
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
    let words = Words {
        before: &args.names,
        dashed: args.dashed,
        after: &args.paths,
    };
    let (rev, paths) = words.revision_then_paths(&repo.tells())?;
    if let (Some(rev), true) = (rev, args.dashed)
        && !repo.names_commit(rev)?
    {
        let kind = if paths.is_empty() { "revision" } else { "tree" };
        return Err(fatal(&format!(
            "Failed to resolve '{rev}' as a valid {kind}."
        )));
    }
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

fn rm<W: Write>(repo: &mut NativeRepo, args: &worktree_args::Rm, out: &mut W) -> Res<()> {
    if args.paths.is_empty() {
        return Err(fatal("No pathspec was given. Which files should I remove?"));
    }
    let how = Rm {
        cached: args.cached,
        recursive: args.recursive,
        force: args.forced,
        ignore_unmatch: args.ignore_unmatch,
        dry_run: args.dry_run,
    };
    let removed = repo.rm(&args.paths, how)?;
    if args.json {
        return report(&removed, true, out);
    }
    for path in removed.iter().filter(|_| !args.quiet) {
        out.write_all(b"rm '")?;
        out.write_all(path)?;
        out.write_all(b"'\n")?;
    }
    Ok(())
}

fn mv<W: Write>(repo: &mut NativeRepo, args: &worktree_args::Mv, out: &mut W) -> Res<()> {
    let Some((destination, sources)) = args.paths.split_last().filter(|(_, s)| !s.is_empty())
    else {
        let usage = "usage: alt mv [-v] [-f] [-n] [-k] <source>... <destination>";
        return Err(crate::machine::Failure::usage(usage).into());
    };
    let how = Mv {
        force: args.forced,
        skip_errors: args.skip_errors,
        dry_run: args.dry_run,
    };
    let renamed = repo.mv(sources, destination, how)?;
    if args.json {
        use crate::json::Json;
        let pair = |r: &crate::native::Renamed| {
            Json::Object(vec![("from", Json::str(&r.from)), ("to", Json::str(&r.to))])
        };
        let renamed = Json::Array(renamed.iter().map(pair).collect());
        return Ok(crate::json::emit(out, vec![("renamed", renamed)])?);
    }
    for r in renamed.iter().filter(|_| args.dry_run) {
        writeln!(out, "Checking rename of '{}' to '{}'", r.from, r.to)?;
    }
    for r in renamed.iter().filter(|_| args.verbose || args.dry_run) {
        writeln!(out, "Renaming {} to {}", r.from, r.to)?;
    }
    Ok(())
}
