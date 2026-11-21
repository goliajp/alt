//! The ref store's error type and the policy gate a transaction passes.

use std::collections::BTreeMap;

use alt_oplog::OpLogError;

use crate::state::Refs;
use crate::{RefChange, RefTarget};

#[derive(Debug)]
pub enum RefError {
    Io(std::io::Error),
    OpLog(OpLogError),
    Format(&'static str),
    Conflict {
        name: String,
    },
    SymrefDepth(String),
    ReservedPayload(u8),
    /// The capability gate denied this transaction. The string names the ref
    /// (or `<read-only>` for a blanket deny) so the operator can act on the
    /// audit message.
    CapabilityDenied(String),
    /// The caller's [`RefPolicy::state_check`] refused the transaction
    /// against the current ref state.
    Rejected(String),
}

impl std::fmt::Display for RefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => f.write_str("io"),
            Self::OpLog(_) => f.write_str("oplog"),
            Self::Format(what) => write!(f, "ref format: {what}"),
            Self::Conflict { name } => write!(f, "ref {name}: expected state does not match"),
            Self::SymrefDepth(name) => write!(f, "symref chain too deep resolving {name}"),
            Self::ReservedPayload(kind) => {
                write!(f, "payload kind {kind} is reserved for ref transactions")
            }
            Self::CapabilityDenied(what) => write!(f, "capability denied: {what}"),
            Self::Rejected(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for RefError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::OpLog(e) => Some(e),
            Self::Format(_)
            | Self::Conflict { .. }
            | Self::SymrefDepth(_)
            | Self::ReservedPayload(_)
            | Self::CapabilityDenied(_)
            | Self::Rejected(_) => None,
        }
    }
}

impl From<std::io::Error> for RefError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<OpLogError> for RefError {
    fn from(e: OpLogError) -> Self {
        Self::OpLog(e)
    }
}

/// Per-call capability gate for ref-affecting transactions. The store invokes
/// it inside the append lock, after the CAS catch-up and before the actual
/// validate, so a denial aborts the transaction with zero on-disk side effect
/// — no oplog write, no idempotency-index update.
///
/// Layered intentionally as a thin callback type so `alt-refs` stays oblivious
/// to where the policy comes from (alt-cli has its own glob / [`Capabilities`]
/// model). The *ref-shaped* gates live here (read-only + branch
/// namespace); force and path gates live in [`NativeRepo`] where ancestry and
/// working-tree paths are already in scope.
///
/// [`Capabilities`]: ../alt_cli/policy/struct.Capabilities.html
/// [`NativeRepo`]: ../alt_cli/native/struct.NativeRepo.html
pub struct RefPolicy<'a> {
    /// If `true`, every ref change is denied.
    pub read_only: bool,
    /// `Some(f)` constrains writable ref names to those `f` accepts. `None`
    /// (the common case: no policy file, or no rule matched) is "any name OK".
    pub is_branch_allowed: Option<&'a dyn Fn(&str) -> bool>,
    /// `Some(f)` vets the transaction against the ref state it would apply
    /// to: `f` sees every ref as it stands after catching up on other
    /// writers, inside the same lock, so the verdict cannot go stale before
    /// the write. `Err(reason)` aborts it as [`RefError::Rejected`].
    pub state_check: Option<&'a StateCheck<'a>>,
}

/// See [`RefPolicy::state_check`].
pub type StateCheck<'a> =
    dyn Fn(&BTreeMap<String, RefTarget>, &[RefChange]) -> Result<(), String> + 'a;

impl RefPolicy<'_> {
    /// The unconstrained policy: any ref, any direction. Pass this (or simply
    /// `None` to [`commit_idempotent`]) when the caller has no policy to apply
    /// — tests, imports, the legacy `commit` shim.
    pub fn full() -> RefPolicy<'static> {
        RefPolicy {
            read_only: false,
            is_branch_allowed: None,
            state_check: None,
        }
    }

    pub(crate) fn check(&self, refs: &Refs, changes: &[RefChange]) -> Result<(), RefError> {
        if self.read_only && !changes.is_empty() {
            return Err(RefError::CapabilityDenied(
                "<read-only>: principal cannot write any ref".into(),
            ));
        }
        if let Some(allow) = self.is_branch_allowed {
            for c in changes {
                if !allow(&c.name) {
                    return Err(RefError::CapabilityDenied(format!(
                        "{}: principal cannot write this ref",
                        c.name
                    )));
                }
            }
        }
        if let Some(f) = self.state_check {
            f(&refs.to_map(), changes).map_err(RefError::Rejected)?;
        }
        Ok(())
    }
}
