//! Appending to a version 2 log.

use std::os::unix::fs::FileExt;

use crate::lock::{lock_exclusive, unlock};
use crate::scan::encode_body;
use crate::v2::compose::{Composer, New};
use crate::v2::frame::{Ext, KIND_CHECKPOINT, KIND_OP, KIND_RECEIPT, REQUIRED, UNDOABLE, UNSYNCED};
use crate::v2::log::Log;
use crate::v2::receipt::Receipt;
use crate::v2::state::State;
use crate::v2::tail::Tail;
use crate::{Framed, Meta, Op, OpId, OpLogError, relaxed_durability};

impl Log {
    /// Refuses for a log this build may read and not write: nothing of
    /// it is changed then, nor what is noted beside it.
    pub(super) fn may_write(&self) -> Result<(), OpLogError> {
        match self.writable {
            true => Ok(()),
            false => Err(OpLogError::Format(
                "the oplog uses a feature this alt does not know to write",
            )),
        }
    }

    /// Refuses where the last frame that says nothing is appended after
    /// it by whoever does not know it is of a kind this build skips.
    pub(super) fn may_append(&self) -> Result<(), OpLogError> {
        let at = self.state.required;
        if at == 0 || at >= self.state.end {
            return Ok(());
        }
        let kind = self.source().at(at, 1)?[0];
        match matches!(kind, KIND_OP | KIND_CHECKPOINT | KIND_RECEIPT) {
            true => Ok(()),
            false => Err(OpLogError::Format(
                "the oplog holds a frame this alt must know to append after it",
            )),
        }
    }

    /// Runs `run` with the exclusive lock on the log taken; it is given
    /// up when `run` is over, also by a panic.
    pub(super) fn locked<T>(&mut self, run: impl FnOnce(&mut Self) -> T) -> std::io::Result<T> {
        struct Locked<'a>(&'a mut Log);
        impl Drop for Locked<'_> {
            fn drop(&mut self) {
                let _ = unlock(&self.0.file);
            }
        }
        lock_exclusive(&self.file)?;
        let locked = Locked(self);
        Ok(run(&mut *locked.0))
    }

    pub fn append_checked<F, E>(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        meta: &Meta,
        build: F,
    ) -> Result<OpId, E>
    where
        F: FnOnce(&[Op]) -> Result<Vec<u8>, E>,
        E: From<OpLogError>,
    {
        self.may_write().map_err(E::from)?;
        if actor.len() > u16::MAX as usize {
            return Err(OpLogError::TooLarge(actor.len()).into());
        }
        let appended = self.locked(|log| {
            log.catch_up_bounded().map_err(E::from)?;
            let payload = build(log.recent())?;
            if u32::try_from(payload.len()).is_err() {
                return Err(OpLogError::TooLarge(payload.len()).into());
            }
            log.write_op(actor, timestamp_ms, &payload, meta)
                .map_err(E::from)
        });
        appended.map_err(OpLogError::from)?
    }

    pub fn append_receipt(&mut self, receipt: &Receipt) -> Result<(), OpLogError> {
        self.may_write()?;
        let body = receipt.encode();
        let flags = if relaxed_durability() { UNSYNCED } else { 0 };
        self.locked(|log| {
            log.catch_up_bounded()?;
            let ext = Ext {
                request: Some(receipt.request),
                ..Ext::default()
            };
            log.write(&New {
                kind: KIND_RECEIPT,
                flags,
                ext,
                body: &body,
                ..New::default()
            })
        })?
    }

    /// Appends the op and takes it into the chain held. The caller holds
    /// the lock and has read the log to its end.
    fn write_op(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
        meta: &Meta,
    ) -> Result<OpId, OpLogError> {
        let body = encode_body(&self.state.head, timestamp_ms, actor, payload);
        let id = OpId(*alt_hash::blake3::hash(&body).as_bytes());
        let mut flags = if relaxed_durability() { UNSYNCED } else { 0 };
        flags |= if meta.undoable { UNDOABLE } else { 0 };
        flags |= if meta.required { REQUIRED } else { 0 };
        let workspace = meta.workspace.unwrap_or_default();
        self.write(&New {
            kind: KIND_OP,
            flags,
            workspace,
            ext: Ext {
                odb: meta.odb,
                inverse_of: meta.inverse_of,
                request: meta.request,
                index_after: meta.index_after,
                step: meta.step,
                retired: meta.retired,
                ..Ext::default()
            },
            body: &body,
            op: Some(id),
            linked: meta.linked,
        })?;
        let op = Op {
            id,
            parent: OpId(body[..32].try_into().unwrap()),
            change_id: [0; 32],
            timestamp_ms,
            actor: actor.to_owned(),
            payload: payload.to_vec(),
            frame: Some(Framed {
                at: self.state.head_at,
                end: self.state.head_end,
                seq: self.state.seq,
                flags,
                workspace,
                inverse_of: meta.inverse_of,
                request: meta.request,
            }),
        };
        if let Some(full) = self.full.get_mut() {
            full.push(op.clone());
        }
        self.ops.push(op);
        Ok(id)
    }

    /// Writes the bytes that append `new` where the log ends, all at once.
    pub(super) fn write(&mut self, new: &New) -> Result<(), OpLogError> {
        self.may_write()?;
        self.may_append()?;
        let composer = self.composer();
        let mut state = self.state.clone();
        let bytes = composer.frame(&mut state, new)?;
        self.put(&bytes, state)
    }

    pub(super) fn composer(&self) -> Composer {
        Composer {
            layout: self.layout,
            boot: self.boot.unwrap_or_default(),
            synced: self.synced,
        }
    }

    /// Writes `bytes` where the log ends; `state` is the log after them.
    pub(super) fn put(&mut self, bytes: &[u8], state: State) -> Result<(), OpLogError> {
        self.file.write_all_at(bytes, self.state.end)?;
        self.closes.copy_from_slice(&bytes[bytes.len() - 4..]);
        self.state = state;
        self.unsynced = true;
        Ok(())
    }

    /// With the lock held: reads the log to its end and sees to the
    /// frames another boot left unsynced that nobody has looked at since,
    /// before a frame is appended after them. A writer that syncs gives
    /// them to whoever syncs what ops name, with the lock given up, and
    /// the log is then recorded as synced up to where it ended before
    /// that. One that does not notes that they are left to be looked at.
    pub(super) fn catch_up_bounded(&mut self) -> Result<(), OpLogError> {
        self.catch_up()?;
        self.check_synced()?;
        if self.judged.get() >= self.state.end || self.vouched_since()? {
            return Ok(());
        }
        self.may_write()?;
        if relaxed_durability() {
            return self.note_left();
        }
        let upto = self.state.end;
        if let Some(sync) = self.named.clone() {
            let left = self.named_from(self.judged.get())?;
            let _ = unlock(&self.file);
            let synced = sync(&left);
            lock_exclusive(&self.file)?;
            synced?;
            self.catch_up()?;
        }
        self.file.sync_all()?;
        let (generation, closes) = (self.state.generation, self.closes);
        let end = self.state.end;
        Tail::update(&self.dir, self.boot, |tail| {
            tail.stands(generation);
            tail.raise(upto, (end == upto).then_some(closes));
            (tail.pending, tail.judged) = (false, end);
        })?;
        self.synced = self.synced.max(upto);
        self.judged.set(u64::MAX);
        self.note_synced(upto);
        Ok(())
    }
}
