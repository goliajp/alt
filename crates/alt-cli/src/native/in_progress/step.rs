//! A command that begins an operation, goes on with it or ends it, taken
//! back as a whole.
//!
//! Such a command changes more than one record tells: several ref
//! transactions and checkouts, and the file that says the operation is
//! under way. It is a step: a mark in the log before the first of its
//! records, and after the last of them one record of what it did in all,
//! the refs, the index and the files from before its first change to
//! after its last, and the state files as they were and as they are.
//! `alt undo` takes that record back and nothing less, so that a merge
//! given up comes back as a merge, and a commit that went on with a pick
//! as the pick.
//!
//! A state file changes only after the log says what it becomes, and the
//! log says with which record of the step that holds. A step cut short is
//! therefore brought to an end from the log alone by the next command
//! that writes: see `recover`. An older alt passes the mark and the record
//! over, and refuses to undo either.

use std::collections::BTreeMap;

use super::super::checkout_recovery::{CheckoutIntent, FileChange, FileState};
use super::super::recovery::{WorkspaceIdentity, decode, encode};
use super::super::*;

mod abandon;
mod guard;
mod mark;
mod recover;
mod undo;

pub(in crate::native) use mark::STEP_MARK;
use mark::StateText;

pub(in crate::native) const STEP: u8 = 0x36;

/// The files next to the index that say an operation is under way.
const STATE_FILES: [&str; 4] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_STATE",
    "REBASE_STATE",
];

#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct StateFile {
    name: String,
    before: Option<String>,
    after: Option<String>,
}

/// A ref from one target to another: a commit id, or `@` and the name of
/// the ref it points to.
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct RefMove {
    name: String,
    old: Option<String>,
    new: Option<String>,
}

#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct StepRecord {
    version: u32,
    workspace: WorkspaceIdentity,
    #[json(default)]
    request_id: Option<String>,
    inverse_of: Option<String>,
    /// the mark the step began with
    #[json(default)]
    begin: Option<String>,
    /// the step was cut short and another program wrote after it: the
    /// record closes the mark and tells nothing
    #[json(default)]
    abandoned: bool,
    refs: Vec<RefMove>,
    checkout: Option<CheckoutIntent>,
    state: Vec<StateFile>,
}

/// What a command has done so far.
#[derive(Default)]
pub(in crate::native) struct Tracked {
    /// each of the state files when the command began
    before: Vec<Option<String>>,
    /// begun in the log before its first record, whatever it plans
    eager: bool,
    inverse_of: Option<String>,
    /// the mark it began with, once that is in the log
    begin: Option<OpId>,
    /// the guard written after that mark
    guard: Option<OpId>,
    refs: Vec<RefChange>,
    checkouts: Vec<CheckoutIntent>,
    /// what state files become with the next record
    planned: Vec<StateText>,
    /// the same, said in the log, until that record is there
    said: Vec<StateText>,
    /// what state files have become
    now: BTreeMap<String, Option<String>>,
    /// those of them whose file is not yet so
    unapplied: Vec<String>,
    /// the intent of a checkout is in the log and its completion is not
    unfinished: bool,
}

/// Each state file that was there `before` or is there now, as it was
/// and as it is.
fn state_changes(
    before: &[Option<String>],
    now: &BTreeMap<String, Option<String>>,
) -> Vec<StateFile> {
    let mut state = Vec::new();
    for (name, before) in STATE_FILES.iter().zip(before) {
        let after = now.get(*name).unwrap_or(before);
        if before.is_some() || after.is_some() {
            state.push(StateFile {
                name: (*name).to_owned(),
                before: before.clone(),
                after: after.clone(),
            });
        }
    }
    state
}

fn written(target: &Option<RefTarget>) -> Option<String> {
    target.as_ref().map(|target| match target {
        RefTarget::Oid(oid) => oid.to_string(),
        RefTarget::Symbolic(name) => format!("@{name}"),
    })
}

fn read(target: &Option<String>) -> Res<Option<RefTarget>> {
    let Some(text) = target else {
        return Ok(None);
    };
    Ok(Some(match text.strip_prefix('@') {
        Some(name) => RefTarget::Symbolic(name.to_owned()),
        None => RefTarget::Oid(text.parse().map_err(|_| "corrupt step record")?),
    }))
}

/// Each ref from what it was before the first change to what the last left.
fn net_refs(changes: &[RefChange]) -> Vec<RefMove> {
    let mut net: Vec<RefChange> = Vec::new();
    for change in changes {
        match net.iter_mut().find(|known| known.name == change.name) {
            Some(known) => known.new = change.new.clone(),
            None => net.push(change.clone()),
        }
    }
    net.retain(|change| change.old != change.new);
    let moved = |change: RefChange| RefMove {
        old: written(&change.old),
        new: written(&change.new),
        name: change.name,
    };
    net.into_iter().map(moved).collect()
}

/// The checkouts one after another as one: each path from what the first
/// found to what the last left.
fn net_checkout(checkouts: Vec<CheckoutIntent>) -> Option<CheckoutIntent> {
    let mut net = checkouts.first()?.clone();
    let mut files: BTreeMap<Vec<u8>, (Option<FileState>, Option<FileState>)> = BTreeMap::new();
    for checkout in checkouts {
        net.index_after = checkout.index_after;
        for change in checkout.files {
            let known = files.entry(change.path);
            known.or_insert((change.before, None)).1 = change.after;
        }
    }
    let changed = files
        .into_iter()
        .filter(|(_, (before, after))| before != after);
    net.files = changed
        .map(|(path, (before, after))| FileChange {
            path,
            before,
            after,
        })
        .collect();
    (!net.files.is_empty() || net.index_before != net.index_after).then_some(net)
}

impl NativeRepo<'_> {
    fn state_path(&self, name: &str) -> PathBuf {
        self.index_path.with_file_name(name)
    }

    fn state_file(&self, name: &str) -> Res<Option<String>> {
        match std::fs::read_to_string(self.state_path(name)) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn state_files(&self) -> Res<Vec<Option<String>>> {
        STATE_FILES
            .iter()
            .map(|name| self.state_file(name))
            .collect()
    }

    /// From here to [`end_step`](Self::end_step), what the command does is
    /// kept track of. An `eager` one is a step from its first record on;
    /// so is one that finds an operation under way; any other becomes one
    /// when it plans a state file.
    pub fn begin_step(&mut self, eager: bool) -> Res<()> {
        let before = self.state_files()?;
        let tracked = Tracked {
            eager: eager || before.iter().any(Option::is_some),
            before,
            ..Default::default()
        };
        *self.step.borrow_mut() = Some(tracked);
        Ok(())
    }

    pub(in crate::native) fn step_refs(&self, changes: &[RefChange]) {
        if let Some(tracked) = self.step.borrow_mut().as_mut() {
            tracked.refs.extend_from_slice(changes);
            tracked.unfinished |= self.checkout_intent.is_some();
        }
        self.step_effected();
    }

    pub(in crate::native) fn step_unfinished(&self, unfinished: bool) {
        if let Some(tracked) = self.step.borrow_mut().as_mut() {
            tracked.unfinished = unfinished;
        }
    }

    pub(in crate::native) fn step_checkout(&self, checkout: &CheckoutIntent) {
        if let Some(tracked) = self.step.borrow_mut().as_mut() {
            tracked.checkouts.push(checkout.clone());
        }
    }

    /// The workspace the step `op` records was done in.
    pub(in crate::native) fn step_workspace(&self, op: &alt_oplog::Op) -> Res<WorkspaceIdentity> {
        Ok(decode::<StepRecord>(&op.payload, STEP)?.workspace)
    }

    /// Closes the mark of a step with the record of what it did, synced
    /// as every last record of a command is: the log is then recorded
    /// as synced to its end, and a command that reads has nothing past
    /// that to look through.
    fn record_step(&mut self, verb: &str, step: &StepRecord) -> Res<()> {
        let actor = self.id.actor(verb);
        let payload = encode(STEP, step)?;
        crash_point!("step-record:before");
        self.store.refs.record_op(&actor, now_ms(), &payload)?;
        crash_point!("step-record:after");
        Ok(())
    }

    /// The end of a command that is kept track of: where it became a
    /// step, the state files as the log says they are, and the record of
    /// all it did. A command that failed in the middle of a checkout
    /// leaves its step without the record, as one that was killed there
    /// does: the next command that writes brings it to an end.
    pub fn end_step(&mut self) -> Res<()> {
        let whole = |tracked: &Tracked| tracked.begin.is_some() && !tracked.unfinished;
        if !self.step.borrow().as_ref().is_some_and(whole) {
            self.step.take();
            return Ok(());
        }
        let applied = self.apply_state(true);
        let tracked = self.step.take().expect("a step was begun");
        applied?;
        if let Some(guard) = tracked.guard {
            self.close_guard(guard)?;
        }
        self.record_step(
            "step",
            &StepRecord {
                version: 1,
                workspace: self.workspace_identity()?,
                request_id: self.request_id.map(|id| crate::machine::hex(&id)),
                inverse_of: tracked.inverse_of,
                begin: tracked.begin.map(|id| id.to_string()),
                abandoned: false,
                refs: net_refs(&tracked.refs),
                checkout: net_checkout(tracked.checkouts),
                state: state_changes(&tracked.before, &tracked.now),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(byte: u8) -> Option<RefTarget> {
        Some(RefTarget::Oid(ObjectId::Sha1([byte; 20])))
    }

    fn change(name: &str, old: Option<RefTarget>, new: Option<RefTarget>) -> RefChange {
        RefChange {
            name: name.to_owned(),
            old,
            new,
        }
    }

    #[test]
    fn refs_are_told_from_their_first_state_to_their_last() {
        let scratch = Some(RefTarget::Symbolic("rebase/default".to_owned()));
        let branch = Some(RefTarget::Symbolic("refs/heads/side".to_owned()));
        let net = net_refs(&[
            change("rebase/default", None, oid(1)),
            change("HEAD", branch.clone(), scratch.clone()),
            change("rebase/default", oid(1), oid(2)),
            change("refs/heads/side", oid(3), oid(2)),
            change("HEAD", scratch, branch),
            change("rebase/default", oid(2), None),
        ]);
        // what was made and removed again, or moved and moved back, is no change
        assert_eq!(net.len(), 1);
        assert_eq!(net[0].name, "refs/heads/side");
        assert_eq!(net[0].old.as_deref(), Some("03".repeat(20).as_str()));
        assert_eq!(read(&net[0].new).unwrap(), oid(2));
        let named = written(&Some(RefTarget::Symbolic("refs/heads/side".to_owned())));
        assert_eq!(named.as_deref(), Some("@refs/heads/side"));
        assert_eq!(read(&named).unwrap(), written_back("refs/heads/side"));
        assert!(read(&Some("no commit".to_owned())).is_err());
    }

    fn written_back(name: &str) -> Option<RefTarget> {
        Some(RefTarget::Symbolic(name.to_owned()))
    }

    fn state(contents: &str) -> Option<FileState> {
        Some(FileState {
            mode: 0o100644,
            contents: contents.to_owned(),
        })
    }

    fn checkout(
        index: (&str, &str),
        files: &[(&str, Option<FileState>, Option<FileState>)],
    ) -> CheckoutIntent {
        let file =
            |(path, before, after): &(&str, Option<FileState>, Option<FileState>)| FileChange {
                path: path.as_bytes().to_vec(),
                before: before.clone(),
                after: after.clone(),
            };
        CheckoutIntent {
            version: 1,
            workspace: WorkspaceIdentity {
                name: "default".to_owned(),
                instance: String::new(),
            },
            request_id: None,
            index_before: index.0.to_owned(),
            index_after: index.1.to_owned(),
            files: files.iter().map(file).collect(),
        }
    }

    #[test]
    fn checkouts_are_told_from_the_first_state_of_a_path_to_its_last() {
        let net = net_checkout(vec![
            checkout(
                ("i0", "i1"),
                &[("a", state("a0"), state("a1")), ("b", None, state("b1"))],
            ),
            checkout(
                ("i1", "i2"),
                &[("a", state("a1"), state("a0")), ("c", state("c0"), None)],
            ),
            checkout(("i2", "i3"), &[("b", state("b1"), state("b2"))]),
        ])
        .unwrap();
        assert_eq!(
            (net.index_before.as_str(), net.index_after.as_str()),
            ("i0", "i3")
        );
        let told: Vec<_> = net
            .files
            .iter()
            .map(|c| (c.path.as_slice(), &c.before, &c.after))
            .collect();
        // `a` is back as it was and no change
        assert_eq!(
            told,
            [
                (&b"b"[..], &None, &state("b2")),
                (&b"c"[..], &state("c0"), &None)
            ]
        );

        // checkouts that end where they began are none
        let there = checkout(("i0", "i1"), &[("a", state("a0"), state("a1"))]);
        let back = checkout(("i1", "i0"), &[("a", state("a1"), state("a0"))]);
        assert!(net_checkout(vec![there, back]).is_none());
        assert!(net_checkout(Vec::new()).is_none());
    }
}
