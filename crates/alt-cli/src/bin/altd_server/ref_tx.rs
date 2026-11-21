//! The ref transaction of a push.

use alt_cli::native::PrincipalKind;
use alt_cli::policy::Capabilities;
use alt_refs::{RefChange, RefError, RefStore, RefTarget};

use super::now_ms;
use super::pack_ingest::Push;

/// The ref transaction of a push, `vet` being its state check.
pub(super) fn commit_ref_updates(
    refs: &mut RefStore,
    caps: &Capabilities,
    push: &Push<'_>,
    vet: &alt_refs::StateCheck<'_>,
) -> Result<(), RefError> {
    let changes: Vec<RefChange> = push
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
    // branch_allow and branch_deny (deny wins), as the local CLI has them
    let is_branch_allowed = |name: &str| {
        if caps.branch_deny.iter().any(|g| g.matches(name)) {
            return false;
        }
        caps.branch_allow.is_empty() || caps.branch_allow.iter().any(|g| g.matches(name))
    };
    let has_constraint = !caps.branch_allow.is_empty() || !caps.branch_deny.is_empty();
    let policy = alt_refs::RefPolicy {
        read_only: caps.read_only,
        is_branch_allowed: match has_constraint {
            true => Some(&is_branch_allowed),
            false => None,
        },
        state_check: Some(vet),
    };
    refs.commit_idempotent(&actor, now_ms(), &changes, None, Some(&policy))?;
    Ok(())
}
