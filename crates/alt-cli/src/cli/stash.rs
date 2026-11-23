//! Running `alt stash`: what its first word asks for.

use std::io::Write;

use super::Res;
use crate::native::{NativeRepo, Stashing};
use crate::worktree_args::Stash;

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
    let entry = match (verb, rest) {
        ("push" | "list" | "clear", [_, ..]) => {
            return Err(usage(&format!("stash {verb} takes no further argument")));
        }
        (_, [_, _, ..]) => return Err(usage("stash takes one entry at most")),
        (_, rest) => rest.first().map(String::as_str),
    };
    if verb != "push" && (args.message.is_some() || args.include_untracked) {
        return Err(usage("-m and -u go with stash push"));
    }
    if verb != "show" && args.patch {
        return Err(usage("-p goes with stash show"));
    }
    if !matches!(verb, "apply" | "pop") && args.index {
        return Err(usage("--index goes with stash apply and stash pop"));
    }
    match verb {
        "push" => repo.stash_push(args.message.as_deref(), args.include_untracked, said, out)?,
        "list" => repo.stash_list(args.json, out)?,
        "show" => repo.stash_show(entry, args.patch, args.json, out)?,
        "drop" => repo.stash_drop(entry, said, out)?,
        "clear" => repo.stash_clear(args.json, out)?,
        "apply" | "pop" => {
            let stopped = repo.stash_apply(entry, args.index, verb == "pop", said, out)?;
            return Ok(u8::from(stopped));
        }
        other => return Err(usage(&format!("unknown stash command '{other}'"))),
    }
    Ok(0)
}
