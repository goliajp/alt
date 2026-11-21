//! A command that begins an operation, goes on with it or ends it, taken
//! back as a whole.
//!
//! Such a command changes more than one record tells: several ref
//! transactions and checkouts, a conflict written without a record, and
//! the file that says the operation is under way. Where that file changed,
//! the command ends with one more record of what it did in all: the refs,
//! the index and the files from before its first change to after its
//! last, and the state files as they were and as they are. `alt undo`
//! takes that record back and nothing less, so that a merge given up comes
//! back as a merge, and a commit that went on with a pick as the pick.
//!
//! The record is written before a state file is removed. An older alt
//! passes it over, and refuses to undo it.

use std::collections::{BTreeMap, HashSet};

use super::super::checkout_recovery::{CheckoutIntent, FileChange, FileState};
use super::super::recovery::{WorkspaceIdentity, canonical_index, decode, encode};
use super::super::*;

mod undo;

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
    refs: Vec<RefMove>,
    checkout: Option<CheckoutIntent>,
    state: Vec<StateFile>,
}

/// What a command has done so far.
#[derive(Default)]
pub(in crate::native) struct Tracked {
    /// each of the state files when the command began
    before: Vec<Option<String>>,
    refs: Vec<RefChange>,
    checkouts: Vec<CheckoutIntent>,
    /// state files to remove once the record is written
    removed: Vec<&'static str>,
}

/// The index and the files at some paths before a write no checkout records.
pub(in crate::native) type Unrecorded = (String, Vec<(BString, Option<FileState>)>);

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

    fn drop_state_file(&self, name: &str) -> Res<()> {
        match crate::durable::remove_file(self.state_path(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// From here to [`end_step`](Self::end_step), what the command does is
    /// kept track of.
    pub fn begin_step(&mut self) -> Res<()> {
        let before = STATE_FILES.iter().map(|name| self.state_file(name));
        let tracked = Tracked {
            before: before.collect::<Res<_>>()?,
            ..Default::default()
        };
        *self.step.borrow_mut() = Some(tracked);
        Ok(())
    }

    /// Removes a state file: at once, or at the end of a command that is
    /// kept track of, after its record is written. Until then the file is
    /// still there to read.
    pub(in crate::native) fn remove_state_file(&self, name: &'static str) -> Res<()> {
        match self.step.borrow_mut().as_mut() {
            Some(tracked) => tracked.removed.push(name),
            None => self.drop_state_file(name)?,
        }
        Ok(())
    }

    pub(in crate::native) fn step_refs(&self, changes: &[RefChange]) {
        if let Some(tracked) = self.step.borrow_mut().as_mut() {
            tracked.refs.extend_from_slice(changes);
        }
    }

    pub(in crate::native) fn step_checkout(&self, checkout: &CheckoutIntent) {
        if let Some(tracked) = self.step.borrow_mut().as_mut() {
            tracked.checkouts.push(checkout.clone());
        }
    }

    fn index_snapshot(&self) -> Res<String> {
        self.snapshot(&canonical_index(self.index()?, self.store.algo()?))
    }

    fn found_at(&self, path: &BString, among: &HashSet<&[u8]>) -> Res<Option<FileState>> {
        self.observed(path, true, false, &|p| among.contains(p))
    }

    /// Before a write to the index and to `paths` that no checkout
    /// records: what is there, where a command is kept track of.
    pub(in crate::native) fn step_unrecorded(&self, paths: &[&BString]) -> Res<Option<Unrecorded>> {
        if self.step.borrow().is_none() {
            return Ok(None);
        }
        let among: HashSet<&[u8]> = paths.iter().map(|path| path.as_slice()).collect();
        let found = |path: &&BString| Ok(((*path).clone(), self.found_at(path, &among)?));
        let files = paths.iter().map(found).collect::<Res<_>>()?;
        Ok(Some((self.index_snapshot()?, files)))
    }

    /// After that write: the change it made, as a checkout would have
    /// recorded it.
    pub(in crate::native) fn step_wrote(&self, before: Option<Unrecorded>) -> Res<()> {
        let Some((index_before, files)) = before else {
            return Ok(());
        };
        let among: HashSet<&[u8]> = files.iter().map(|(path, _)| path.as_slice()).collect();
        let mut changes = Vec::new();
        for (path, before) in &files {
            let after = self.found_at(path, &among)?;
            if *before != after {
                changes.push(FileChange {
                    path: path.to_vec(),
                    before: before.clone(),
                    after,
                });
            }
        }
        self.step_checkout(&CheckoutIntent {
            version: 1,
            workspace: self.workspace_identity()?,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before,
            index_after: self.index_snapshot()?,
            files: changes,
        });
        Ok(())
    }

    /// The workspace the step `op` records was done in.
    pub(in crate::native) fn step_workspace(&self, op: &alt_oplog::Op) -> Res<WorkspaceIdentity> {
        Ok(decode::<StepRecord>(&op.payload, STEP)?.workspace)
    }

    fn record_step(&mut self, step: &StepRecord) -> Res<()> {
        let actor = self.id.actor("step");
        let payload = encode(STEP, step)?;
        self.store.refs.record_op(&actor, now_ms(), &payload)?;
        // durable before a state file it tells of is removed
        self.store.sink()?.fsync_all()?;
        Ok(())
    }

    /// The end of a command that is kept track of: where it changed a state
    /// file, the record of all it did, and then the removals it left for
    /// the end.
    pub fn end_step(&mut self) -> Res<()> {
        let Some(tracked) = self.step.take() else {
            return Ok(());
        };
        let mut state = Vec::new();
        for (name, before) in STATE_FILES.iter().zip(tracked.before) {
            let after = match tracked.removed.contains(name) {
                true => None,
                false => self.state_file(name)?,
            };
            if before != after {
                let name = (*name).to_owned();
                state.push(StateFile {
                    name,
                    before,
                    after,
                });
            }
        }
        if !state.is_empty() {
            self.record_step(&StepRecord {
                version: 1,
                workspace: self.workspace_identity()?,
                request_id: self.request_id.map(|id| crate::machine::hex(&id)),
                inverse_of: None,
                refs: net_refs(&tracked.refs),
                checkout: net_checkout(tracked.checkouts),
                state,
            })?;
        }
        for name in tracked.removed {
            self.drop_state_file(name)?;
        }
        Ok(())
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
