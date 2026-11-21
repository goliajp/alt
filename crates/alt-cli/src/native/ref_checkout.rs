//! A ref transaction that a checkout goes with: the transaction carries
//! the intent of the checkout, so that the two are done or not done as one.

use super::*;

impl NativeRepo<'_> {
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
