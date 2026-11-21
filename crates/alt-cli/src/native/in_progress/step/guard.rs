//! What keeps an alt from before steps from writing in a workspace while a
//! step is open there.
//!
//! Such an alt passes the mark of a step over. It knows the intent of a
//! checkout, and refuses to write while one of the workspace is without
//! its completion. So a step puts one in the log right after its mark, and
//! its completion right before the record that closes the step: the intent
//! of a checkout that changes nothing, from and to an index no workspace
//! can have, which that alt cannot finish either. That index is what tells
//! a guard from any other intent. Until the step is closed,
//! by the command or by the recovery of this alt, the older one refuses
//! every command that writes; from then on it finds a checkout that was
//! completed, and works on.

use super::super::super::checkout_recovery::{CheckoutIntent, PREPARE_CHECKOUT};
use super::super::super::recovery::{canonical_index, encode, hash};
use super::super::super::*;

/// The index a guard is from and to: one path under the control directory,
/// which is in no index a workspace has.
fn guard_index(algo: HashAlgo) -> Vec<u8> {
    let entry = IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode: 0o100644,
        uid: 0,
        gid: 0,
        size: 0,
        oid: ObjectId::hash_object(algo, ObjectKind::Blob, b""),
        flags: 9,
        extended_flags: None,
        path: BString::from(".alt/step"),
    };
    let index = Index {
        version: 2,
        entries: vec![entry],
        extensions: Vec::new(),
    };
    canonical_index(index, algo)
}

impl NativeRepo<'_> {
    /// Whether `intent` is the guard of a step: it is from and to the
    /// index kept for guards, which is told by what it holds and not by
    /// the intent changing nothing. A checkout that gives a step up may
    /// have nothing to change either, and is no guard.
    pub(in crate::native) fn is_guard(&self, intent: &CheckoutIntent) -> Res<bool> {
        if !intent.files.is_empty() || intent.index_before != intent.index_after {
            return Ok(false);
        }
        Ok(intent.index_before == hash(&guard_index(self.store.algo()?)))
    }

    /// Writes the guard of the step this command begins. Not synced: it
    /// is durable with the record of the step that comes next.
    pub(super) fn open_guard(&mut self) -> Res<OpId> {
        let index = self.snapshot(&guard_index(self.store.algo()?))?;
        let guard = CheckoutIntent {
            version: 1,
            workspace: self.workspace_identity()?,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: index.clone(),
            index_after: index,
            files: Vec::new(),
        };
        let actor = self.id.actor("step");
        let payload = encode(PREPARE_CHECKOUT, &guard)?;
        let refs = &mut self.store.refs;
        Ok(refs.record_op_unsynced(&actor, now_ms(), &payload)?)
    }

    /// Closes the guard, right before the record that closes its step.
    pub(super) fn close_guard(&mut self, guard: OpId) -> Res<()> {
        let payload = self.checkout_done(guard)?;
        let actor = self.id.actor("step");
        let refs = &mut self.store.refs;
        refs.record_op_unsynced(&actor, now_ms(), &payload)?;
        Ok(())
    }
}
