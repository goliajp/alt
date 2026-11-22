//! What a step did to the notes ref, taken back.
//!
//! Every workspace writes the notes of its commits to the one notes ref,
//! each on the tip it finds. A step that makes several commits moves the
//! ref several times, and a commit of another workspace may lie between
//! two of them or after the last. So the ref is not moved back to where
//! the step found it unless nobody else has moved it since then: anywhere
//! else the notes the step wrote are taken out on top of the tip, and
//! every other note stays.

use super::super::super::recovery::RefContext;
use super::super::super::*;
use super::StepRecord;
use crate::meta::{self, NOTES_REF};

fn commit_of(target: &Option<RefTarget>) -> Res<Option<ObjectId>> {
    match target {
        Some(RefTarget::Oid(commit)) => Ok(Some(*commit)),
        Some(RefTarget::Symbolic(_)) => Err("the notes ref names another ref".into()),
        None => Ok(None),
    }
}

impl NativeRepo<'_> {
    /// The changes the ref transactions of the step `record` closes made
    /// to the notes ref, in their order.
    pub(super) fn step_notes(
        &self,
        record: &alt_oplog::Op,
        step: &StepRecord,
    ) -> Res<Vec<RefChange>> {
        let ops = self.store.refs.oplog().ops()?;
        let at = |id: &str| ops.iter().position(|op| op.id.to_string() == id);
        let begin = step.begin.as_deref().and_then(at);
        let end = at(&record.id.to_string());
        let (Some(begin), Some(end)) = (begin, end) else {
            return Err("missing step mark".into());
        };
        let mut notes = Vec::new();
        for op in &ops[begin..end] {
            let Some(tx) = alt_refs::parse_tx(&op.payload)? else {
                continue;
            };
            let Some(context) = tx.context else { continue };
            let context: RefContext = alt_json::from_slice(&context)?;
            if context.workspace == step.workspace {
                notes.extend(tx.changes.into_iter().filter(|c| c.name == NOTES_REF));
            }
        }
        Ok(notes)
    }

    /// The change of the notes ref that takes back those among `changes`
    /// a step made to it one after another; none where it made none, or
    /// none of its notes is left.
    pub(super) fn notes_back(&mut self, changes: &[RefChange]) -> Res<Option<RefChange>> {
        let own: Vec<&RefChange> = changes.iter().filter(|c| c.name == NOTES_REF).collect();
        let (Some(first), Some(last)) = (own.first(), own.last()) else {
            return Ok(None);
        };
        let tip = self.store.refs.get(NOTES_REF).cloned();
        let moved = |new: Option<RefTarget>| RefChange {
            name: NOTES_REF.to_owned(),
            old: tip.clone(),
            new,
        };
        // nobody else has moved the ref since the step first did
        if own.windows(2).all(|two| two[0].new == two[1].old) && tip == last.new {
            return Ok((first.old != tip).then(|| moved(first.old.clone())));
        }
        let mut made = Vec::new();
        for change in &own {
            made.push((commit_of(&change.old)?, commit_of(&change.new)?));
        }
        // another writer flushes its objects before it moves the ref
        self.store.odb.refresh()?;
        let (name, email) = self.id.sig();
        let signature = format!("{name} <{email}> {} +0000", now_ms() / 1000);
        let algo = self.store.algo()?;
        let odb = std::cell::RefCell::new(&mut self.store.odb);
        let read = |id: ObjectId| -> meta::ReadResult {
            Ok(odb.borrow().get(&id)?.map(|o| (o.kind, o.data.to_vec())))
        };
        let mut write = |kind: ObjectKind, data: &[u8]| -> meta::WriteResult {
            let id = ObjectId::hash_object(algo, kind, data);
            odb.borrow_mut().put(id, kind, data)?;
            Ok(id)
        };
        let on = commit_of(&tip)?;
        let rebuilt = meta::take_back(&read, &mut write, algo, on, &made, &signature)?;
        self.store.odb.flush()?;
        Ok(rebuilt.map(|commit| moved(Some(RefTarget::Oid(commit)))))
    }
}
