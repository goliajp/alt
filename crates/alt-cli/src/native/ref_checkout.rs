//! A ref transaction that a checkout goes with: the transaction carries
//! the intent of the checkout, so that the two are done or not done as one.

use super::*;

impl NativeRepo<'_> {
    /// The single gate every CLI ref-changing op must go through: enforces
    /// read-only and force/non-FF *before* entering the ref store, then asks
    /// the store to do the atomic CAS-append with the namespace gate folded
    /// in (via [`RefPolicy`]). A denial — at any axis — costs no on-disk
    /// state. All subtransactions carry the request id in their context.
    pub(super) fn commit_refs(&mut self, verb: &str, changes: &[RefChange]) -> Res<OpId> {
        self.commit_refs_with(verb, changes, None, recovery::RefEffects::Unknown)
    }

    pub(super) fn commit_refs_only(&mut self, verb: &str, changes: &[RefChange]) -> Res<OpId> {
        self.commit_refs_with(verb, changes, None, recovery::RefEffects::RefsOnly)
    }

    pub(super) fn commit_refs_with(
        &mut self,
        verb: &str,
        changes: &[RefChange],
        key: Option<IdemKey>,
        effects: recovery::RefEffects,
    ) -> Res<OpId> {
        self.ensure_writable(verb)?;
        self.commit_refs_gated(verb, changes, key, effects)
    }

    /// [`commit_refs_with`](Self::commit_refs_with) past the check that
    /// nothing waits to be recovered: for what recovers.
    pub(super) fn commit_refs_gated(
        &mut self,
        verb: &str,
        changes: &[RefChange],
        key: Option<IdemKey>,
        effects: recovery::RefEffects,
    ) -> Res<OpId> {
        self.lock_workspace()?;
        self.ensure_no_force(changes)?;
        let allow = self.caps.branch_allow.clone();
        let deny = self.caps.branch_deny.clone();
        let has_constraint = !allow.is_empty() || !deny.is_empty();
        let is_branch_allowed = move |name: &str| {
            // deny wins over allow
            if deny.iter().any(|g| g.matches(name)) {
                return false;
            }
            allow.is_empty() || allow.iter().any(|g| g.matches(name))
        };
        let own_head = self.head_ref.clone();
        let one_workspace_per_branch = move |refs: &_, changes: &[RefChange]| {
            workspace::one_workspace_per_branch(&own_head, refs, changes)
        };
        let policy = RefPolicy {
            read_only: self.caps.read_only,
            is_branch_allowed: if has_constraint {
                Some(&is_branch_allowed)
            } else {
                None
            },
            state_check: Some(&one_workspace_per_branch),
        };
        let actor = self.id.actor(verb);
        let context = self.ref_context(effects)?;
        self.step_effect()?;
        crash_point!("ref-transaction:before");
        let id = self.store.refs.commit_scoped(
            &actor,
            now_ms(),
            changes,
            key,
            Some(&policy),
            Some(&context),
        )?;
        crash_point!("ref-transaction:after");
        self.step_refs(changes);
        // opt-in op-level signing. Best-effort — a missing sec key
        // logs nothing and is not fatal (signing is a per-repo capability,
        // not a hard requirement). Verification at read time tells the
        // auditor which ops are signed and which aren't.
        self.maybe_sign_op(id)?;
        Ok(id)
    }

    /// What a ref transaction of this command says of itself in the log.
    pub(super) fn ref_context(&self, effects: recovery::RefEffects) -> Res<Vec<u8>> {
        Ok(alt_json::to_vec(&recovery::RefContext {
            version: 2,
            effects: if self.checkout_intent.is_some() {
                recovery::RefEffects::Checkout
            } else {
                effects
            },
            workspace: self.workspace_identity()?,
            inverse_of: self.inverse_of.clone(),
            checkout: self.checkout_intent.clone(),
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
        })?)
    }

    pub(super) fn commit_refs_checkout(
        &mut self,
        verb: &str,
        changes: &[RefChange],
        old: &[WorkEntry],
        target: &[WorkEntry],
    ) -> Res<OpId> {
        self.lock_workspace()?;
        self.ensure_no_pending_recovery()?;
        self.preflight_checkout(old, target)?;
        let intent = self.prepare_checkout(old, target)?;
        self.checkout_intent = Some(intent.clone());
        let result = self.commit_refs(verb, changes);
        self.checkout_intent = None;
        let operation = result?;
        self.apply_prepared_checkout(operation, &intent)?;
        Ok(operation)
    }

    /// Advances `branch` from `old` to `new` in one ref transaction.
    pub(super) fn advance_branch_checkout(
        &mut self,
        branch: &str,
        old: ObjectId,
        new: ObjectId,
        before: &[WorkEntry],
        after: &[WorkEntry],
    ) -> Res<()> {
        self.commit_refs_checkout(
            "merge",
            &[RefChange {
                name: branch.to_owned(),
                old: Some(RefTarget::Oid(old)),
                new: Some(RefTarget::Oid(new)),
            }],
            before,
            after,
        )?;
        Ok(())
    }
}
