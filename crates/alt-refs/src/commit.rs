//! A ref transaction: appended to the log, then finished in this store.
//!
//! Once it is appended the transaction is in the log, where every other
//! process reads it, whatever fails afterwards. What a caller stored for
//! it must stay from then on, and a failure there is told apart from one
//! that left the log as it was ([`RefError::Recorded`]).

use crate::{IdemKey, OpId, RefChange, RefError, RefPolicy, RefStore, tx};

impl RefStore {
    /// Applies one atomic transaction: all expected-old values must match
    /// the current state or nothing happens (no op is recorded). On success
    /// the transaction is one durable op; returns its id.
    pub fn commit(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        changes: &[RefChange],
    ) -> Result<OpId, RefError> {
        self.commit_idempotent(actor, timestamp_ms, changes, None, None)
    }

    /// Like [`commit`](Self::commit) but stamps the transaction with a client
    /// idempotency `key`, recorded durably with the op. A later write carrying
    /// the same key is detected via [`applied_request`](Self::applied_request)
    /// and not applied again — the basis of the daemon's exactly-once retry.
    ///
    /// An optional [`RefPolicy`] gate runs **inside** the append lock, after
    /// the CAS catch-up but before validation — a denied transaction aborts
    /// with zero on-disk side effect (no oplog write, no idempotency entry).
    /// `policy = None` is the unconstrained path used by tests, imports, and
    /// the legacy [`commit`](Self::commit) shim; production CLI callers pass
    /// `Some(&policy)` so the chokepoint enforces the capability gate.
    pub fn commit_idempotent(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        changes: &[RefChange],
        key: Option<IdemKey>,
        policy: Option<&RefPolicy<'_>>,
    ) -> Result<OpId, RefError> {
        self.commit_scoped(actor, timestamp_ms, changes, key, policy, None)
    }

    /// A ref transaction with caller-owned recovery context, committed in the
    /// same log frame as the ref changes. Legacy callers omit the context.
    ///
    /// An error other than [`RefError::Recorded`] left the log as it was.
    pub fn commit_scoped(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        changes: &[RefChange],
        key: Option<IdemKey>,
        policy: Option<&RefPolicy<'_>>,
        context: Option<&[u8]>,
    ) -> Result<OpId, RefError> {
        let op = self.append_tx(actor, timestamp_ms, changes, key, policy, context)?;
        match self.finish_tx(op, changes, key) {
            Ok(()) => Ok(op),
            Err(cause) => Err(RefError::Recorded {
                op,
                cause: Box::new(cause),
            }),
        }
    }

    /// The first half of [`commit_scoped`](Self::commit_scoped): the
    /// transaction is appended to the log, or an error left the log as it
    /// was. [`finish_tx`](Self::finish_tx) is the other half.
    pub fn append_tx(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        changes: &[RefChange],
        key: Option<IdemKey>,
        policy: Option<&RefPolicy<'_>>,
        context: Option<&[u8]>,
    ) -> Result<OpId, RefError> {
        // The validate → append must be atomic against other writers: inside
        // the append lock, fold in any ops they committed since (so `refs` is
        // the true current state), then CAS-validate this transaction against
        // it. A rejected transaction aborts with nothing written.
        self.append_op(actor, timestamp_ms, |refs| {
            // Policy gate first: a CapabilityDenied here aborts the
            // append (no oplog row, no idempotency update) — denial
            // costs nothing observable.
            if let Some(p) = policy {
                p.check(refs, changes)?;
            }
            refs.check(changes, false)?;
            Ok(match context {
                Some(context) => tx::encode_scoped_tx(changes, key, context),
                None => tx::encode_tx(changes, key),
            })
        })
    }

    /// Makes the transaction `op`, appended by [`append_tx`](Self::append_tx)
    /// with these `changes` and `key`, durable and folds it into this store.
    /// The transaction is in the log whether or not this fails.
    pub fn finish_tx(
        &mut self,
        op: OpId,
        changes: &[RefChange],
        key: Option<IdemKey>,
    ) -> Result<(), RefError> {
        self.oplog.sync()?;
        // our own op is durable now; fold it into the map, the cursor, and the
        // idempotency index (so a retry with the same key is detected)
        self.refs.apply(changes, false)?;
        if let Some(k) = key {
            self.by_key.insert(k, op);
        }
        self.applied = self.oplog.recent().len();
        self.note_op()
    }
}
