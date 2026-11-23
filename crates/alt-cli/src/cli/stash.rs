//! Running `alt stash`: what its first word asks for.

use std::io::Write;

use super::Res;
use crate::native::{NativeRepo, StashPush as Push, Stashing};
use crate::worktree_args::Stash;

const VERBS: [&str; 7] = ["push", "list", "show", "apply", "pop", "drop", "clear"];

fn usage(said: &str) -> Box<dyn std::error::Error> {
    crate::machine::Failure::usage(said).into()
}

/// The exit status is 1 where an apply or a pop stopped in conflict.
pub(super) fn run<W: Write>(repo: &mut NativeRepo, args: &Stash, out: &mut W) -> Res<u8> {
    let said = Stashing {
        quiet: args.quiet,
        json: args.json,
    };
    let (verb, rest) = match args.words.split_first() {
        Some((verb, rest)) => (verb.as_str(), rest),
        None => ("push", &args.words[..]),
    };
    if !VERBS.contains(&verb) {
        // git's words for a first word that is none of its commands
        let said = format!(
            "fatal: subcommand wasn't specified; 'push' can't be assumed due to unexpected \
             token '{verb}'\n"
        );
        return Err(crate::native::said::refused(&said, 128));
    }
    if verb == "push" {
        if args.patch {
            return Err(usage(
                "stash push -p is not supported: alt does not pick hunks one by one; name the \
                 paths to put away instead, as in `alt stash push -- <path>`",
            ));
        }
        let paths: Vec<String> = rest.iter().chain(&args.paths).cloned().collect();
        let push = Push {
            message: args.message.as_deref(),
            untracked: args.include_untracked,
            paths: &paths,
        };
        repo.stash_push(push, said, out)?;
        return Ok(0);
    }
    if !args.paths.is_empty() {
        return Err(usage("paths go with stash push"));
    }
    let entry = match (verb, rest) {
        ("list" | "clear", [_, ..]) => {
            return Err(usage(&format!("stash {verb} takes no further argument")));
        }
        (_, [_, _, ..]) => return Err(usage("stash takes one entry at most")),
        (_, rest) => rest.first().map(String::as_str),
    };
    if args.message.is_some() || args.include_untracked {
        return Err(usage("-m and -u go with stash push"));
    }
    if verb != "show" && args.patch {
        return Err(usage("-p goes with stash show"));
    }
    if !matches!(verb, "apply" | "pop") && args.index {
        return Err(usage("--index goes with stash apply and stash pop"));
    }
    match verb {
        "list" => repo.stash_list(args.json, out)?,
        "show" => repo.stash_show(entry, args.patch, args.json, out)?,
        "drop" => repo.stash_drop(entry, said, out)?,
        "clear" => repo.stash_clear(args.json, out)?,
        "apply" | "pop" => {
            let stopped = repo.stash_apply(entry, args.index, verb == "pop", said, out)?;
            return Ok(u8::from(stopped));
        }
        _ => unreachable!("the verb is one of the known ones"),
    }
    Ok(0)
}
