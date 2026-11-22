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
        let later = matches!(
            entry.id,
            "renormalize"
                | "forced"
                | "update"
                | "all"
                | "continue_pick"
                | "abort_pick"
                | "abort_merge"
        );
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
    let subcommand = matches.subcommand().map(|sub| {
        json!({
            "name": sub.name(), "arguments": canonical_matches(sub)
        })
    });
    json!({"values": args, "subcommand": subcommand})
}
