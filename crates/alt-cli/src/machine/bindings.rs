//! What the arguments of a command come to for the request that records it.

use alt_args::Matches;
use alt_json::{Value, json};

use super::{Parsed, hex, matches_json};

impl Parsed {
    /// The arguments in the one form every spelling of them shares. Only a
    /// write binds its request to it, so it is built on demand.
    pub fn normalized(&self) -> Value {
        canonical_matches(&self.matches)
    }
}

/// The flags that came after bindings were first recorded.
const LATER: &[&str] = &[
    "renormalize",
    "forced",
    "update",
    "all",
    "continue_pick",
    "abort_pick",
    "abort_merge",
    "noted",
    "as_it_is",
    "no_ff",
    "ff_only",
    "oneline",
    "name_only",
    "name_status",
    "stat",
    "show_toplevel",
    "show_git_dir",
    "absolute_git_dir",
    "show_prefix",
    "is_inside_work_tree",
    "abbrev_ref",
    "symbolic_full_name",
    "verify_one",
    "quiet",
    "author_given",
    "author_date",
    "reset_author",
    "exec",
    "reset_author_date",
    "rebase_merges",
    "force_move",
    "rename",
    "rename_over",
    "create_over",
    "tag_made",
    "no_tag",
];

/// The argument values a durable request is bound to. Stored bindings are
/// compared with this on a retry, so its shape must never change.
fn canonical_matches(matches: &Matches) -> Value {
    let mut args = std::collections::BTreeMap::new();
    for entry in matches.entries() {
        if matches!(entry.id, "request_id" | "workspace") {
            continue;
        }
        // a global flag reads the same on every level, wherever it was given
        if entry.id == "json" {
            let raw = if matches_json(matches) {
                "true"
            } else {
                "false"
            };
            args.insert(entry.id, vec![hex(raw.as_bytes())]);
            continue;
        }
        // a flag that came after bindings were first recorded binds only
        // when it is set: the commands recorded before it keep theirs
        let later = LATER.contains(&entry.id);
        if later && entry.values.iter().all(|v| v.as_bytes() == b"false") {
            continue;
        }
        args.insert(
            entry.id,
            entry
                .values
                .iter()
                .map(|value| hex(value.as_bytes()))
                .collect::<Vec<_>>(),
        );
    }
    // nothing else tells `reset main --` from `reset main`
    if matches.dashes_alone() {
        args.insert("--", Vec::new());
    }
    let subcommand = matches.subcommand().map(|sub| {
        json!({
            "name": sub.name(), "arguments": canonical_matches(sub)
        })
    });
    json!({"values": args, "subcommand": subcommand})
}
