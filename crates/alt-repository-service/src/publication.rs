use crate::{Error, Graph, Node, Result, graph};
use alt_git_codec::ObjectId;
use alt_remote_protocol::{Attribution, Limits, ObjectFormat, Publication, Scope};
use std::collections::{BTreeMap, BTreeSet};

/// Produced by the authentication adapter, never deserialized from a request.
/// The transaction must compare its generation/revocation values with authority.
#[derive(Clone, Debug)]
pub struct Authorization {
    pub scope: Scope,
    pub runtime_generation: u64,
    pub actor_id: String,
    pub attribution: Attribution,
    pub expires_at: u64,
    pub revocation_generation: u64,
    pub grants: Vec<Grant>,
    pub publish_records: bool,
}
pub use alt_remote_protocol::authorization::{Grant, RefSelector};
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefPolicy {
    pub refs: RefSelector,
    pub allow_force: bool,
    pub allow_delete: bool,
    pub require_review: bool,
}
impl RefPolicy {
    pub fn validate(&self) -> Result<()> {
        Ok(self.refs.validate()?)
    }
}

/// Read under the repository's transaction lock. Review approvals are populated
/// only by current task/evidence validation bound to the exact publication digest.
pub struct RepositorySnapshot {
    pub scope: Scope,
    pub runtime_generation: u64,
    pub revocation_generation: u64,
    pub policy_version: u64,
    pub object_format: ObjectFormat,
    pub refs: BTreeMap<String, ObjectId>,
    pub policies: Vec<RefPolicy>,
    pub approved_publications: BTreeSet<String>,
    pub writes_enabled: bool,
}
/// A validated snapshot result. It does not itself mutate refs or authorize
/// later reuse: the SQL adapter runs validation again inside its transaction.
pub struct ValidatedPublication {
    pub digest: String,
    pub policy_version: u64,
    pub updates: Vec<ValidatedRef>,
}
pub struct ValidatedRef {
    pub name: String,
    pub expected: Option<ObjectId>,
    pub new: Option<ObjectId>,
}

/// Cheap admission check before reserving upload space. Final validation still
/// checks object closure, ancestry, path scope and current review policy.
pub fn authorize_intent(
    publication: &Publication,
    snapshot: &RepositorySnapshot,
    auth: &Authorization,
    limits: Limits,
    now: u64,
) -> Result<String> {
    let digest = publication.digest(limits)?;
    for grant in &auth.grants {
        grant.validate()?;
    }
    for policy in &snapshot.policies {
        policy.validate()?;
    }

    if publication.key.scope != snapshot.scope
        || auth.scope != snapshot.scope
        || auth.runtime_generation != snapshot.runtime_generation
        || auth.revocation_generation != snapshot.revocation_generation
        || auth.expires_at <= now
        || auth.actor_id != publication.key.actor_id
        || auth.attribution != publication.attribution
        || !snapshot.writes_enabled
        || snapshot.policy_version == 0
        || snapshot.runtime_generation == 0
    {
        return Err(Error::Forbidden("authorization scope, lease or generation"));
    }
    if publication.object_format != snapshot.object_format {
        return Err(Error::Forbidden("repository object format"));
    }
    if !publication.record_roots.is_empty() && !auth.publish_records {
        return Err(Error::Forbidden("task publication capability"));
    }
    for update in &publication.refs {
        if !auth.grants.iter().any(|grant| {
            grant.refs.matches(&update.name)
                && (update.expected.is_some() || grant.create)
                && (update.expected.is_none() || update.new.is_none() || grant.update)
                && (update.new.is_some() || grant.delete)
                && (!update.force || grant.force)
        }) {
            return Err(Error::Forbidden("reference admission capability"));
        }
    }
    Ok(digest)
}

pub fn validate_publication(
    publication: &Publication,
    snapshot: &RepositorySnapshot,
    auth: &Authorization,
    graph: &(impl Graph + ?Sized),
    limits: Limits,
    now: u64,
) -> Result<ValidatedPublication> {
    let digest = authorize_intent(publication, snapshot, auth, limits, now)?;
    let mut budget = graph::budget(graph);
    let mut resulting = snapshot.refs.clone();
    let mut updates = Vec::new();
    for update in &publication.refs {
        let parse = |value: &Option<String>| {
            value
                .as_deref()
                .map(|value| {
                    value
                        .parse::<ObjectId>()
                        .map_err(|_| Error::Integrity("reference object id"))
                })
                .transpose()
        };
        let expected = parse(&update.expected)?;
        let new = parse(&update.new)?;
        if snapshot.refs.get(&update.name).copied() != expected {
            return Err(Error::Precondition(update.name.clone()));
        }
        if let Some(oid) = new {
            let node = graph.node(oid)?;
            if update.name.starts_with("refs/heads/")
                && !matches!(node.as_ref(), Node::Commit { .. })
            {
                return Err(Error::Integrity("branch target is not a commit"));
            }
        }
        let non_fast_forward = match (expected, new) {
            (Some(old), Some(new)) => {
                if update.name.starts_with("refs/tags/") {
                    true
                } else if matches!(graph.node(old)?.as_ref(), Node::Commit { .. })
                    && matches!(graph.node(new)?.as_ref(), Node::Commit { .. })
                {
                    !graph::is_ancestor_bounded(graph, old, new, &mut budget)?
                } else {
                    true
                }
            }
            _ => false,
        };
        if non_fast_forward && !update.force {
            return Err(Error::Forbidden("force intent required"));
        }
        for policy in snapshot
            .policies
            .iter()
            .filter(|p| p.refs.matches(&update.name))
        {
            if (update.force && !policy.allow_force) || (new.is_none() && !policy.allow_delete) {
                return Err(Error::Forbidden("protected reference policy"));
            }
            if policy.require_review && !snapshot.approved_publications.contains(&digest) {
                return Err(Error::Forbidden("current review required"));
            }
        }
        let mut allowed = false;
        for grant in auth.grants.iter().filter(|g| g.refs.matches(&update.name)) {
            if (expected.is_none() && !grant.create)
                || (expected.is_some() && new.is_some() && !grant.update)
                || (new.is_none() && !grant.delete)
                || (update.force && !grant.force)
            {
                continue;
            }
            if let Some(paths) = &grant.paths {
                if !update.name.starts_with("refs/heads/") {
                    continue;
                }
                if !path_changes_allowed(graph, expected, new, paths, &mut budget)? {
                    continue;
                }
            }
            allowed = true;
            break;
        }
        if !allowed {
            return Err(Error::Forbidden("reference or path capability"));
        }
        if let Some(oid) = new {
            resulting.insert(update.name.clone(), oid);
        } else {
            resulting.remove(&update.name);
        }
        updates.push(ValidatedRef {
            name: update.name.clone(),
            expected,
            new,
        });
    }
    // SQL has no filesystem D/F conflict, but Git clients and portable exports
    // require a namespace that can be represented without ambiguous refs.
    for name in resulting.keys() {
        for (index, _) in name.match_indices('/') {
            if resulting.contains_key(&name[..index]) {
                return Err(Error::Forbidden("reference namespace collision"));
            }
        }
    }
    Ok(ValidatedPublication {
        digest,
        policy_version: snapshot.policy_version,
        updates,
    })
}
fn path_allowed(path: &[u8], prefixes: &[Vec<u8>]) -> bool {
    prefixes.iter().any(|prefix| {
        !prefix.is_empty()
            && !prefix.starts_with(b"/")
            && !prefix.ends_with(b"/")
            && !prefix
                .split(|b| *b == b'/')
                .any(|p| p.is_empty() || p == b"." || p == b"..")
            && (path == prefix
                || path
                    .strip_prefix(prefix.as_slice())
                    .is_some_and(|suffix| suffix.starts_with(b"/")))
    })
}
fn path_changes_allowed(
    graph: &(impl Graph + ?Sized),
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    paths: &[Vec<u8>],
    budget: &mut crate::graph::WalkBudget,
) -> Result<bool> {
    if graph::changed_paths_bounded(graph, old, new, budget)?
        .iter()
        .any(|p| !path_allowed(p, paths))
    {
        return Ok(false);
    }
    // Checking tips alone lets an attacker change a forbidden file and revert
    // it before push. Check every newly introduced commit against all parents.
    let mut old_history = BTreeSet::new();
    let mut pending: Vec<_> = old.into_iter().collect();
    while let Some(oid) = pending.pop() {
        budget.step()?;
        if !old_history.insert(oid) {
            continue;
        }
        let node = graph.node(oid)?;
        let Node::Commit { parents, .. } = node.as_ref() else {
            return Err(Error::Integrity("old branch target"));
        };
        pending.extend(parents);
    }
    let mut seen = BTreeSet::new();
    let mut pending: Vec<_> = new.into_iter().collect();
    while let Some(oid) = pending.pop() {
        budget.step()?;
        if old_history.contains(&oid) || !seen.insert(oid) {
            continue;
        }
        let node = graph.node(oid)?;
        let Node::Commit { parents, .. } = node.as_ref() else {
            return Err(Error::Integrity("new branch target"));
        };
        let bases: Vec<_> = if parents.is_empty() {
            vec![None]
        } else {
            parents.iter().copied().map(Some).collect()
        };
        for base in bases {
            if graph::changed_paths_bounded(graph, base, Some(oid), budget)?
                .iter()
                .any(|p| !path_allowed(p, paths))
            {
                return Ok(false);
            }
        }
        pending.extend(parents);
    }
    Ok(true)
}
