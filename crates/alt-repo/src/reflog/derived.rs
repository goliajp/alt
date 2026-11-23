//! The log of a ref as the operations of a native store tell it: every
//! ref transaction says what each ref it moves named before and names
//! after, so the values a ref went through are read from them in order.

use std::collections::HashMap;

use alt_git_codec::ObjectId;
use alt_oplog::OpId;
use alt_refs::{RefStore, RefTarget};

use super::{Move, Said};
use crate::RepoError;

/// The moves of `want` the log of the store holds.
pub(super) struct Derived {
    pub moves: Vec<Move>,
    /// the ref was deleted on the way: its log began anew
    pub anew: bool,
}

/// What an actor string says the command was.
fn verb_of(actor: &str) -> &str {
    if let Some(rest) = actor.strip_prefix("cli/") {
        return rest.split('@').next().unwrap_or(rest);
    }
    let said = actor.split(';').find_map(|part| part.strip_prefix("verb:"));
    said.unwrap_or("")
}

/// The ref a head ref is on, where it is on one, and the commit it is at.
type At = (Option<String>, Option<ObjectId>);

fn at(state: &HashMap<String, RefTarget>, head: &str) -> At {
    let commit = |name: &str| match state.get(name) {
        Some(RefTarget::Oid(oid)) => Some(*oid),
        _ => None,
    };
    match state.get(head) {
        Some(RefTarget::Symbolic(on)) => (Some(on.clone()), commit(on)),
        _ => (None, commit(head)),
    }
}

fn oid(target: &Option<RefTarget>) -> Option<ObjectId> {
    match target {
        Some(RefTarget::Oid(oid)) => Some(*oid),
        _ => None,
    }
}

/// Reads the moves of `want` from the ops after `after`, from all of them
/// where there is no such op to begin after. An import is no move: what
/// git logged of the refs it brings is kept beside the store.
pub(super) fn derived(
    refs: &RefStore,
    want: &str,
    head: bool,
    after: Option<Option<OpId>>,
) -> Result<Derived, RepoError> {
    let mut state: HashMap<String, RefTarget> = HashMap::new();
    // where each rebase under way began: its scratch ref and the new base
    let mut bases: HashMap<String, ObjectId> = HashMap::new();
    let mut counting = !matches!(after, Some(Some(_)));
    let mut found = Derived {
        moves: Vec::new(),
        anew: false,
    };
    for op in refs.oplog().ops()? {
        let Some(tx) = alt_refs::parse_tx(&op.payload)? else {
            counting |= after == Some(Some(op.id));
            continue;
        };
        if !counting {
            apply(&mut state, &tx.changes);
            counting = after == Some(Some(op.id));
            continue;
        }
        let verb = verb_of(&op.actor);
        let before = at(&state, want);
        let moved = if head {
            before.0.as_deref()
        } else {
            Some(want)
        };
        let tips = matches!(verb, "merge" | "flow").then(|| tips(&state, moved));
        apply(&mut state, &tx.changes);
        // a rebase ends where its scratch ref goes
        let ended = tx.changes.iter().filter(|c| c.new.is_none());
        let onto = ended.fold(None, |onto, c| onto.or(bases.remove(&c.name)));
        if verb == "rebase" {
            for c in tx.changes.iter().filter(|c| !c.name.starts_with("refs/")) {
                if let (None, Some(base)) = (&c.old, oid(&c.new)) {
                    bases.insert(c.name.clone(), base);
                }
            }
        }
        if verb == "import" {
            continue;
        }
        let told = |old, new: ObjectId, from, to| Move {
            old,
            new: Some(new),
            by: None,
            at_ms: Some(op.timestamp_ms),
            said: Said::Of {
                verb: verb.to_owned(),
                from,
                to,
                onto,
                tip: tips.as_ref().and_then(|tips| only(tips, new)),
            },
        };
        if head {
            let now = at(&state, want);
            if let (true, Some(new)) = (before != now, now.1) {
                let on = (before.0 != now.0).then_some((before.0, now.0));
                let (from, to) = on.unwrap_or_default();
                found.moves.push(told(before.1, new, from, to));
            }
            continue;
        }
        for c in tx.changes.iter().filter(|c| c.name == want) {
            match &c.new {
                Some(RefTarget::Oid(new)) => found.moves.push(told(oid(&c.old), *new, None, None)),
                Some(RefTarget::Symbolic(_)) => {}
                None => {
                    found.moves.clear();
                    found.anew = true;
                }
            }
        }
    }
    Ok(found)
}

/// The branches and remote-tracking branches of `state` but `moved`,
/// each by its short name with the commit it is at.
fn tips(state: &HashMap<String, RefTarget>, moved: Option<&str>) -> Vec<(String, ObjectId)> {
    let tip = |(name, target): (&String, &RefTarget)| {
        let short = name
            .strip_prefix("refs/heads/")
            .or_else(|| name.strip_prefix("refs/remotes/"))?;
        match target {
            RefTarget::Oid(oid) if Some(name.as_str()) != moved => Some((short.to_owned(), *oid)),
            _ => None,
        }
    };
    state.iter().filter_map(tip).collect()
}

/// The name of the one of `tips` at `commit`, where there is but one.
fn only(tips: &[(String, ObjectId)], commit: ObjectId) -> Option<String> {
    let mut at = tips.iter().filter(|(_, tip)| *tip == commit);
    match (at.next(), at.next()) {
        (Some((name, _)), None) => Some(name.clone()),
        _ => None,
    }
}

fn apply(state: &mut HashMap<String, RefTarget>, changes: &[alt_refs::RefChange]) {
    for c in changes {
        match &c.new {
            Some(new) => state.insert(c.name.clone(), new.clone()),
            None => state.remove(&c.name),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::verb_of;

    #[test]
    fn the_command_is_read_from_both_forms_of_an_actor() {
        assert_eq!(verb_of("human:alice;user:alice;verb:commit"), "commit");
        assert_eq!(
            verb_of("agent:x;session:s;user:u;verb:cherry-pick"),
            "cherry-pick"
        );
        assert_eq!(verb_of("cli/switch@alice"), "switch");
        assert_eq!(verb_of("alice"), "");
    }
}
