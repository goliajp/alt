//! Checks on one command level once all of its words are read.

use crate::error::Error;
use crate::matches::Matched;
use crate::scope::{Scope, Slot};
use crate::spec::{Kind, Preset};
use crate::usage;

/// The ids of the arguments given on the command line, in order of first
/// appearance; group entries are left out.
pub(crate) fn given<'a>(scope: &Scope, entries: &'a [Matched]) -> Vec<&'a str> {
    let explicit = entries.iter().filter(|entry| entry.explicit);
    let ids = explicit.map(|entry| entry.id);
    ids.filter(|id| scope.find(id).is_some()).collect()
}

/// What `slot` may not appear together with, as it declares it.
fn excluded(scope: &Scope, slot: &Slot) -> Vec<&'static str> {
    let mut ids = slot.arg.conflicts.to_vec();
    if !slot.arg.group.is_empty() {
        let peers = scope.slots.iter().map(|peer| peer.arg);
        let peers = peers.filter(|peer| peer.group == slot.arg.group && peer.id != slot.arg.id);
        ids.extend(peers.map(|peer| peer.id));
    }
    ids
}

/// The given arguments `id` conflicts with, by either side's declaration.
fn clashes<'a>(scope: &Scope, id: &str, given: &[&'a str]) -> Vec<&'a str> {
    let own = scope
        .find(id)
        .map_or(Vec::new(), |slot| excluded(scope, slot));
    let mut found = Vec::new();
    for other in given.iter().filter(|other| **other != id) {
        let theirs = scope
            .find(other)
            .map_or(Vec::new(), |slot| excluded(scope, slot));
        if (own.contains(other) || theirs.contains(&id)) && !found.contains(other) {
            found.push(*other);
        }
    }
    found
}

fn conflicts(scope: &Scope, given: &[&str]) -> Result<(), Error> {
    for id in given {
        let clashing = clashes(scope, id, given);
        if clashing.is_empty() {
            continue;
        }
        let show = |id: &&str| scope.find(id).map(|slot| slot.arg.display(None));
        let kept: Vec<&str> = given
            .iter()
            .filter(|id| !clashing.contains(id))
            .copied()
            .collect();
        let mut used: Vec<&str> = Vec::new();
        for slot in kept.iter().filter_map(|id| scope.find(id)) {
            let needs = slot.arg.requires.iter();
            used.extend(needs.filter(|id| !kept.contains(id) && !clashing.contains(id)));
        }
        used.extend(&kept);
        let slot = scope.find(id).expect("given ids are arguments");
        return Err(Error::conflict(
            slot.arg,
            &clashing.iter().filter_map(show).collect::<Vec<_>>(),
            usage::narrowed(scope, &usage::required(scope), &used),
        ));
    }
    Ok(())
}

fn required(scope: &Scope, given: &[&str]) -> Result<(), Error> {
    let mut needed = usage::required(scope);
    for slot in given.iter().filter_map(|id| scope.find(id)) {
        for id in slot.arg.requires {
            if !needed.contains(id) {
                needed.push(id);
            }
        }
    }
    let absent = |id: &str| !given.contains(&id);
    let mut missing: Vec<&str> = needed.iter().copied().filter(|id| absent(id)).collect();
    missing.retain(|id| clashes(scope, id, given).is_empty());
    for slot in scope.slots.iter().filter(|slot| absent(slot.arg.id)) {
        let unless = slot.arg.required_unless;
        if !unless.is_empty() && unless.iter().all(|id| absent(id)) {
            missing.push(slot.arg.id);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    // the positionals before a missing one are missing as well
    let position = |id: &str| scope.positionals().position(|(_, slot)| slot.arg.id == id);
    let last = missing.iter().filter_map(|id| position(id)).max();
    for (_, slot) in scope.positionals().take(last.unwrap_or(0)) {
        if absent(slot.arg.id) {
            missing.push(slot.arg.id);
        }
    }
    let mut options = Vec::new();
    let mut positionals = Vec::new();
    for slot in &scope.slots {
        let listed = needed.iter().chain(&missing).any(|id| *id == slot.arg.id);
        if !listed || !absent(slot.arg.id) {
            continue;
        }
        match slot.arg.is_positional() {
            true => positionals.push(slot.arg.display(Some(true))),
            false => options.push((
                needed
                    .iter()
                    .chain(&missing)
                    .position(|id| *id == slot.arg.id),
                slot.arg.display(None),
            )),
        }
    }
    options.sort_by_key(|(at, _)| *at);
    let mut lines: Vec<String> = options.into_iter().map(|(_, text)| text).collect();
    lines.extend(positionals);
    let used: Vec<&str> = given.iter().chain(&missing).copied().collect();
    Err(Error::missing_arguments(
        &lines,
        usage::narrowed(scope, &needed, &used),
    ))
}

fn defaults(scope: &Scope, entries: &mut Vec<Matched>) {
    for slot in &scope.slots {
        if entries.iter().any(|entry| entry.id == slot.arg.id) {
            continue;
        }
        let value = match (slot.arg.kind, &slot.arg.preset) {
            (Kind::Help | Kind::Version, _) => continue,
            (Kind::Flag, _) => "false".to_owned(),
            (_, Preset::Text(text)) => (*text).to_owned(),
            (_, Preset::Computed(compute)) => compute(),
            (_, Preset::None) => continue,
        };
        entries.push(Matched {
            id: slot.arg.id,
            values: vec![value],
            explicit: false,
        });
    }
}

/// Rejects conflicting and missing arguments, then fills in defaults.
pub(crate) fn finish(scope: &Scope, entries: &mut Vec<Matched>) -> Result<(), Error> {
    let given = given(scope, entries);
    conflicts(scope, &given)?;
    required(scope, &given)?;
    defaults(scope, entries);
    Ok(())
}
