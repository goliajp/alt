//! The ref transaction of a push.

use alt_refs::{OpId, RefChange, RefError, RefPolicy, RefStore, RefTarget, StateCheck};

use super::Push;
use crate::native::PrincipalKind;
use crate::policy::Capabilities;

pub(super) struct Tx {
    changes: Vec<RefChange>,
    actor: String,
    /// of each principal the push is held to: what all of them allow
    caps: Vec<Capabilities>,
}

impl Tx {
    pub(super) fn new(caps: Vec<Capabilities>, push: &Push<'_>) -> Self {
        let changes = push
            .updates
            .iter()
            .map(|u| RefChange {
                name: u.name.clone(),
                old: u.old.map(RefTarget::Oid),
                new: u.new.map(RefTarget::Oid),
            })
            .collect();
        let actor = format!(
            "{kind}:{id};verb:wire/receive-pack;sig:{sig}",
            kind = match push.principal.kind {
                PrincipalKind::Human => "human",
                PrincipalKind::Agent => "agent",
            },
            id = push.principal.id,
            sig = push.sig_label,
        );
        Tx {
            changes,
            actor,
            caps,
        }
    }

    fn with_policy<T>(
        &self,
        state_check: Option<&StateCheck<'_>>,
        run: impl FnOnce(&RefPolicy<'_>) -> T,
    ) -> T {
        // branch_allow and branch_deny (deny wins), as the local CLI has them
        let allows = |caps: &Capabilities, name: &str| {
            if caps.branch_deny.iter().any(|g| g.matches(name)) {
                return false;
            }
            caps.branch_allow.is_empty() || caps.branch_allow.iter().any(|g| g.matches(name))
        };
        let is_branch_allowed = |name: &str| self.caps.iter().all(|caps| allows(caps, name));
        let constrains =
            |caps: &Capabilities| !caps.branch_allow.is_empty() || !caps.branch_deny.is_empty();
        let has_constraint = self.caps.iter().any(constrains);
        run(&RefPolicy {
            read_only: self.caps.iter().any(|caps| caps.read_only),
            is_branch_allowed: match has_constraint {
                true => Some(&is_branch_allowed),
                false => None,
            },
            state_check,
        })
    }

    /// Whether the pusher may write the refs at all, which no ref tells.
    pub(super) fn names_allowed(&self) -> Result<(), RefError> {
        self.with_policy(None, |policy| policy.check_names(&self.changes))
    }

    /// Appends the transaction, `state` being its state check. An error
    /// left the log as it was.
    pub(super) fn append(
        &self,
        refs: &mut RefStore,
        state: &StateCheck<'_>,
    ) -> Result<OpId, RefError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        self.with_policy(Some(state), |policy| {
            refs.append_tx(&self.actor, now, &self.changes, None, Some(policy), None)
        })
    }

    /// What is left of the transaction once it is appended as `op`.
    pub(super) fn finish(&self, refs: &mut RefStore, op: OpId) -> Result<(), RefError> {
        refs.finish_tx(op, &self.changes, None)
    }
}
