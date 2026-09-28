//! Commit metadata on the native store: every recorded commit gets a note
//! under [`meta::NOTES_REF`], written in the same ref transaction as the
//! branch move, and `alt meta` reads or edits notes afterwards.

use super::*;
use crate::meta::{self, Facts, Meta};

/// What the caller records about a commit on top of what is derived.
#[derive(Debug, Default, Clone)]
pub struct MetaInput {
    /// One-line decisions (at most [`meta::MAX_DECISIONS`]).
    pub decisions: Vec<String>,
    /// Extra `key=value` fields, stored as `x-key`.
    pub extra: Vec<(String, String)>,
    /// The commit this one replaces (amend, rebase): its recorded fields and
    /// replacement message carry over.
    pub carry_from: Option<ObjectId>,
}

impl NativeRepo<'_> {
    /// The note to write for a commit with `message` and `tree` on `parents`.
    pub(super) fn meta_for(
        &self,
        message: &str,
        tree: ObjectId,
        parents: &[ObjectId],
        input: &MetaInput,
    ) -> Res<Meta> {
        let before = match parents.first() {
            Some(&p) => self.commit_entries(p)?,
            None => Vec::new(),
        };
        let after = flatten_tree(&self.store.odb, tree, self.store.algo)?;
        let old: std::collections::HashMap<&BString, (ObjectId, u32)> =
            before.iter().map(|e| (&e.path, (e.oid, e.mode))).collect();
        let mut touched: Vec<String> = after
            .iter()
            .filter(|e| old.get(&e.path) != Some(&(e.oid, e.mode)))
            .map(|e| e.path.to_string())
            .collect();
        let kept: std::collections::HashSet<&BString> = after.iter().map(|e| &e.path).collect();
        touched.extend(
            before
                .iter()
                .filter(|e| !kept.contains(&e.path))
                .map(|e| e.path.to_string()),
        );
        touched.sort();
        let principal = &self.id.principal;
        let controlling = std::env::var("ALT_CONTROLLING")
            .ok()
            .filter(|v| !v.is_empty());
        let mut note = meta::derive(
            message,
            Facts {
                author_type: principal.kind.as_str(),
                author: &principal.id,
                controlling: controlling.as_deref(),
                session: principal.session.as_deref(),
                touched,
            },
            &input.decisions,
            &input.extra,
        )?;
        if let Some(from) = input.carry_from
            && let Some(old_note) = self.read_meta(from)?
        {
            meta::carry_over(&old_note, &mut note);
        }
        Ok(note)
    }

    /// The note on `commit`, if it has one.
    pub(super) fn read_meta(&self, commit: ObjectId) -> Res<Option<Meta>> {
        let Some(tip) = self.store.refs.resolve(meta::NOTES_REF)? else {
            return Ok(None);
        };
        let odb = &self.store.odb;
        let read = |id: ObjectId| -> meta::ReadResult {
            Ok(odb.get(&id)?.map(|o| (o.kind, o.data.to_vec())))
        };
        meta::find(&read, self.store.algo, tip, commit)
    }

    /// Writes `note` for `target` on the current notes tip and returns the
    /// ref change that publishes it.
    pub(super) fn note_change(&mut self, target: ObjectId, note: &Meta) -> Res<RefChange> {
        let parent = self.store.refs.resolve(meta::NOTES_REF)?;
        let (name, email) = self.id.sig();
        let signature = format!("{name} <{email}> {} +0000", now_ms() / 1000);
        let algo = self.store.algo;
        let odb = std::cell::RefCell::new(&mut self.store.odb);
        let read = |id: ObjectId| -> meta::ReadResult {
            Ok(odb.borrow().get(&id)?.map(|o| (o.kind, o.data.to_vec())))
        };
        let mut write = |kind: ObjectKind, data: &[u8]| -> meta::WriteResult {
            let id = ObjectId::hash_object(algo, kind, data);
            odb.borrow_mut().put(id, kind, data)?;
            Ok(id)
        };
        let commit = meta::store(&read, &mut write, algo, parent, target, note, &signature)?;
        self.store.odb.flush()?;
        Ok(RefChange {
            name: meta::NOTES_REF.to_owned(),
            old: parent.map(RefTarget::Oid),
            new: Some(RefTarget::Oid(commit)),
        })
    }

    /// Applies `changes` together with `note` for `target` in one ref
    /// transaction. Concurrent commits contend only for the notes ref; when
    /// another writer moved it first, the note is rebuilt on the new tip and
    /// the transaction tried again.
    pub(super) fn commit_refs_with_note(
        &mut self,
        verb: &str,
        changes: &[RefChange],
        target: ObjectId,
        note: &Meta,
    ) -> Res<()> {
        loop {
            let mut all = changes.to_vec();
            all.push(self.note_change(target, note)?);
            match self.commit_refs(verb, &all) {
                Ok(_) => return Ok(()),
                Err(e) if notes_moved(&*e) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// `alt meta show <rev>`: the note on a commit.
    pub fn meta_show(&self, rev: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let commit = self.rev(rev)?;
        let note = self
            .read_meta(commit)?
            .ok_or_else(|| format!("no metadata recorded for {commit}"))?;
        if json {
            use crate::json::Json;
            let mut fields: Vec<(&'static str, Json)> =
                vec![("commit", Json::str(commit.to_string()))];
            let pairs = note
                .fields
                .iter()
                .map(|(k, v)| Json::Array(vec![Json::str(k), Json::str(v)]))
                .collect();
            fields.push(("fields", Json::Array(pairs)));
            fields.push((
                "message",
                note.message.as_deref().map_or(Json::Null, Json::str),
            ));
            crate::json::emit(out, fields)?;
        } else {
            write!(out, "{}", note.render())?;
        }
        Ok(())
    }

    /// `alt meta set <rev> [-m <msg>] [--decision …] [--field k=v …]`: amend a
    /// commit's note — replace its message, add decisions or fields — without
    /// touching the commit.
    pub fn meta_set(
        &mut self,
        rev: &str,
        message: Option<&str>,
        decisions: &[String],
        extra: &[(String, String)],
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("meta set")?;
        let commit = self.rev(rev)?;
        let mut note = self.read_meta(commit)?.unwrap_or_default();
        if note.fields.is_empty() {
            note.fields.push(("schema".into(), meta::SCHEMA.into()));
        }
        let decided = note.all("decision").count() + decisions.len();
        if decided > meta::MAX_DECISIONS {
            return Err(format!("at most {} decisions per commit", meta::MAX_DECISIONS).into());
        }
        for (k, v) in meta::recorded(decisions, extra)? {
            if k == "decision" {
                note.fields.push((k, v));
            } else {
                note.set(&k, &v);
            }
        }
        if let Some(m) = message {
            note.message = Some(if m.ends_with('\n') {
                m.to_owned()
            } else {
                format!("{m}\n")
            });
        }
        self.commit_refs_with_note("meta", &[], commit, &note)?;
        writeln!(out, "metadata updated for {commit}")?;
        Ok(())
    }
}

/// Whether a failed ref transaction lost the race for the notes ref only.
fn notes_moved(e: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        e.downcast_ref::<alt_refs::RefError>(),
        Some(alt_refs::RefError::Conflict { name }) if name == meta::NOTES_REF
    )
}
