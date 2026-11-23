//! Appending to a version 2 log, and reading what other writers appended.

use std::os::unix::fs::FileExt;

use crate::lock::{lock_exclusive, lock_shared, unlock};
use crate::scan::encode_body;
use crate::v2::compose::{Composer, New};
use crate::v2::frame::{Ext, KIND_OP, KIND_RECEIPT, REQUIRED, UNDOABLE, UNSYNCED, View};
use crate::v2::log::Log;
use crate::v2::open::op_of;
use crate::v2::receipt::Receipt;
use crate::v2::salvage::{Salvaged, salvage};
use crate::v2::scan::Stop;
use crate::v2::state::State;
use crate::v2::stream::walk_file;
use crate::v2::tail::Tail;
use crate::{Discarded, Framed, Meta, Op, OpId, OpLogError, relaxed_durability};

impl Log {
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
        if !self.writable {
            return Err(OpLogError::Format(
                "the oplog uses a feature this alt does not know to write",
            ));
        }
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

    pub fn refresh(&mut self) -> Result<(), OpLogError> {
        self.locked(|log| log.catch_up())?
    }

    pub fn follow(&mut self) -> Result<(), OpLogError> {
        lock_shared(&self.file)?;
        let read = self.read_on(false);
        let _ = unlock(&self.file);
        read
    }

    /// Reads the frames other writers appended and cuts off what a
    /// crashed one left at the end; the caller holds the exclusive lock.
    pub(super) fn catch_up(&mut self) -> Result<(), OpLogError> {
        self.read_on(true)
    }

    /// Whether the last frame this handle holds is still where the handle
    /// has it, in a file of `size` bytes: a cut takes it away, and what is
    /// appended after reads otherwise.
    pub(super) fn holds_head(&self, size: u64) -> Result<bool, OpLogError> {
        if self.state.last == 0 {
            return Ok(true);
        }
        if size < self.state.end {
            return Ok(false);
        }
        Ok(self.source().at(self.state.end - 4, 4)? == self.closes)
    }

    /// The log through the frames that follow the ones held in a file of
    /// `size` bytes, their ops, and, where what follows them is no frame,
    /// what still reads after it.
    fn read_past(&self, size: u64) -> Result<(State, Vec<Op>, Option<Salvaged>), OpLogError> {
        let source = self.source();
        let mut read = |at: u64, len: usize| source.at(at, len);
        let mut state = self.state.clone();
        let mut added = Vec::new();
        let mut each = |at: u64, end: u64, view: &View| {
            if view.head.kind == KIND_OP {
                added.push(op_of(at, end, view)?);
            }
            Ok(())
        };
        let stop = walk_file(&mut read, &self.layout, &mut state, size, false, &mut each)?;
        let found = match stop {
            Stop::Unread => {
                let (end, generation) = (state.end, state.generation);
                Some(salvage(&mut read, &self.layout, end, size, generation)?)
            }
            Stop::End => None,
        };
        Ok((state, added, found))
    }

    /// Takes in the frames past the ones held, under a lock on the log.
    /// What follows the last of them that is no frame is cut off the file
    /// when `cut`, under the exclusive lock, and stays in it otherwise.
    fn read_on(&mut self, cut: bool) -> Result<(), OpLogError> {
        let size = self.file.metadata()?.len();
        if !self.holds_head(size)? {
            return Err(OpLogError::CutBack);
        }
        if size <= self.state.end || (!cut && self.hidden == Some(size)) {
            return Ok(());
        }
        let (state, added, found) = self.read_past(size)?;
        let synced = self.tail().map_or(0, |tail| tail.synced);
        let claim = found.as_ref().map_or(0, |found| found.claim);
        if found.is_some() && state.end < synced.max(self.synced).max(state.synced).max(claim) {
            return Err(OpLogError::Format("oplog record corrupt"));
        }
        if state.end > self.state.end {
            let closes = self.source().at(state.end - 4, 4)?;
            self.closes.copy_from_slice(&closes);
        }
        self.state = state;
        if let Some(full) = self.full.get_mut() {
            full.extend_from_slice(&added);
        }
        self.ops.extend(added);
        self.passed = !cut && found.is_some();
        if let Some(found) = found.filter(|_| cut) {
            let discarded = Discarded {
                from: self.state.end,
                to: size,
                unread: (size - self.state.end).saturating_sub(found.covered),
                ops: found.ops,
            };
            self.cut_end(discarded)?;
        }
        if cut {
            self.hidden = None;
        }
        Ok(())
    }

    /// With the lock held: reads the log to its end and, for a writer
    /// that syncs, sees to it that every frame it holds was vouched for
    /// before it appends after them. Frames another boot left unsynced
    /// that nobody has looked at since are given to whoever syncs what
    /// ops name, with the lock given up, and the log is then recorded as
    /// synced up to where it ended before that.
    pub(super) fn catch_up_bounded(&mut self) -> Result<(), OpLogError> {
        self.catch_up()?;
        self.check_synced()?;
        if relaxed_durability() || self.judged.get() >= self.state.end {
            return Ok(());
        }
        let upto = self.state.end;
        if let Some(sync) = self.named.clone() {
            let left = self.ops_from(self.judged.get());
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
            tail.generation = generation;
            tail.raise(upto, (end == upto).then_some(closes));
            (tail.judged, tail.hidden) = (end, 0);
        })?;
        self.synced = self.synced.max(upto);
        self.judged.set(u64::MAX);
        self.note_synced(upto);
        Ok(())
    }

    /// The ops held whose frames start at or after `from`.
    pub(super) fn ops_from(&self, from: u64) -> Vec<Op> {
        let after = |op: &&Op| op.frame.is_some_and(|frame| frame.at >= from);
        self.ops.iter().filter(after).cloned().collect()
    }
}
