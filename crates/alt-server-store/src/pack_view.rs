//! The view a Git request works from: who is asking, what they may do, and the
//! repository's references and live segments as of one instant.

use crate::Jsonb;
use crate::{
    Credential, Error, Refs, Result, Store,
    store::{instance, integer, unsigned},
};
use alt_pack_segment::SegmentSet;
use alt_remote_protocol::Scope;

mod led;
pub use led::FOUND_ALL;
pub(crate) use led::led;
mod statements;
use statements::{VIEW_ALL, VIEW_BOUNDED, VIEW_MANY_TIPS, VIEW_NOTHING, VIEW_PREFIXES, VIEW_TIPS};

/// more wanted object ids than this are given to the view as a subquery
pub const ARRAY_TIPS: usize = 2000;

/// A read request on a repository with more references than this names the ones it
/// needs before any are read.
pub const DEFERRED_REFS: usize = 4096;

/// which references a view is opened with
#[derive(Clone, Copy, Debug)]
pub enum RefFilter<'a> {
    All,
    /// none: the request does not look at references, and the view says they are deferred
    Nothing,
    /// all of them, or none yet when there are more than `DEFERRED_REFS`
    Bounded,
    /// the names under any of these prefixes, and the default branch
    Prefixes(&'a [String]),
    /// the references at any of these object ids; with `tags`, every tag too
    Tips {
        wants: &'a [String],
        tags: bool,
    },
}

/// which of the repository's references a view holds
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefScope {
    Complete,
    /// The ones the filter named. `other_tips` tells whether some reference points at
    /// an object the filter did not name.
    Filtered {
        other_tips: bool,
    },
    /// none: the caller opens the view again with the filter of its request
    Deferred,
}

pub struct PackView {
    pub scope: Scope,
    pub refs: Refs,
    pub ref_scope: RefScope,
    pub default_branch: String,
    pub segments: SegmentSet,
    /// the ids of those segments, in their order: what a push that was verified
    /// against them is published against, or verified anew
    pub segment_ids: Vec<String>,
    /// some stored objects may be unreachable from the refs
    pub unreachable: bool,
    /// what is kept of the objects the refs lead to is kept under this
    pub(crate) led_under: String,
    pub actor_id: String,
    pub attribution: alt_remote_protocol::Attribution,
    /// writes are enabled and the caller holds some ref-changing grant
    pub writable: bool,
}

impl Store {
    /// authenticates a token and opens the repository view in the same transaction
    pub fn git_open(
        &mut self,
        token: &str,
        repository: &str,
        filter: RefFilter<'_>,
    ) -> Result<(Credential, PackView)> {
        if token.len() != 69 || !token.starts_with("alt1_") {
            return Err(Error::Unauthorized);
        }
        let credential = Credential {
            digest: crate::store::token_digest(token),
        };
        let view = self.pack_view_with(&credential, repository, filter)?;
        Ok((credential, view))
    }

    /// refs and live segments as of one instant, for a caller allowed to read the repository
    pub fn pack_view(&mut self, credential: &Credential, repository: &str) -> Result<PackView> {
        self.pack_view_with(credential, repository, RefFilter::All)
    }

    /// The same view with the references the filter names. The caller is authorized
    /// and the references and segments are read as of one instant, whatever the filter.
    pub fn pack_view_with(
        &mut self,
        credential: &Credential,
        repository: &str,
        filter: RefFilter<'_>,
    ) -> Result<PackView> {
        match self.pack_view_once(credential, repository, filter) {
            // a merge retired segments between the list being read and the files
            // being opened, and their time on disk ran out: the new list has none of them
            Err(Error::Integrity("pack segment unavailable")) => {
                self.pack_view_once(credential, repository, filter)
            }
            view => view,
        }
    }

    fn pack_view_once(
        &mut self,
        credential: &Credential,
        repository: &str,
        filter: RefFilter<'_>,
    ) -> Result<PackView> {
        if self.spec.deployment_mode == alt_remote_protocol::DeploymentMode::SelfHosted
            && let Some(view) = self.pack_view_statement(credential, repository, filter)?
        {
            return Ok(view);
        }
        // every reference: a complete view answers any filter
        crate::reading::twice(|| self.pack_view_locked(credential, repository))
    }

    /// The same decision as `pack_view_locked` from one statement and therefore one
    /// snapshot, without row locks. Delegated credentials return None and take the
    /// transactional path, which owns the delegation rules.
    fn pack_view_statement(
        &mut self,
        credential: &Credential,
        repository: &str,
        filter: RefFilter<'_>,
    ) -> Result<Option<PackView>> {
        use alt_json::Value;
        let digest = &credential.digest;
        let row = match filter {
            RefFilter::All => self.client.query_one(VIEW_ALL, &[digest, &repository]),
            RefFilter::Nothing => self.client.query_one(VIEW_NOTHING, &[digest, &repository]),
            RefFilter::Bounded => self.client.query_one(VIEW_BOUNDED, &[digest, &repository]),
            RefFilter::Prefixes(prefixes) => self
                .client
                .query_one(VIEW_PREFIXES, &[digest, &repository, &prefixes]),
            RefFilter::Tips { wants, tags } => self.client.query_one_for_values(
                if wants.len() > ARRAY_TIPS {
                    VIEW_MANY_TIPS
                } else {
                    VIEW_TIPS
                },
                &[digest, &repository, &wants, &tags],
            ),
        }?;
        let field = |index: usize| row.get::<_, Option<Jsonb>>(index).map(|json| json.0);
        let text =
            |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let number = |value: &Value, key: &str| value.get(key).and_then(Value::as_i64);
        let flag = |value: &Value, key: &str| value.get(key).and_then(Value::as_bool);
        let instance = field(0).ok_or(Error::Precondition("instance not initialized"))?;
        if text(&instance, "installation_id").as_deref() != Some(self.spec.installation_id.as_str())
            || text(&instance, "workspace_id").as_deref() != Some(self.spec.workspace_id.as_str())
            || text(&instance, "deployment_mode").as_deref() != Some("self_hosted")
            || number(&instance, "runtime_generation")
                != Some(integer(self.spec.runtime_generation)?)
            || text(&instance, "signing_key_id").as_deref()
                != Some(
                    alt_hash::blake3::hash(self.signing_key.verifying_key().as_bytes())
                        .to_hex()
                        .as_str(),
                )
        {
            return Err(Error::Precondition(
                "instance identity, mode, generation or signing key",
            ));
        }
        if row.get::<_, bool>(2) {
            return Ok(None);
        }
        let time: i64 = row.get(9);
        let credential_row = field(1).ok_or(Error::Unauthorized)?;
        let attribution = credential_row
            .get("attribution")
            .cloned()
            .unwrap_or(Value::Null);
        if text(&credential_row, "identity_source").as_deref() != Some("self_hosted")
            || flag(&credential_row, "revoked") != Some(false)
            || number(&credential_row, "expires_at").is_none_or(|expires| expires <= time)
            || attribution.get("kind").and_then(Value::as_str) == Some("agent")
        {
            return Err(Error::Unauthorized);
        }
        if field(3).is_some_and(|account| flag(&account, "disabled") != Some(false)) {
            return Err(Error::Unauthorized);
        }
        if let Some(session) = field(4)
            && (number(&session, "account_generation") != number(&session, "generation")
                || number(&session, "expires_at").is_none_or(|expires| expires <= time))
        {
            return Err(Error::Unauthorized);
        }
        crate::telemetry::authenticated_as(&attribution);
        let repository_row = field(6).ok_or(Error::Unauthorized)?;
        if text(&repository_row, "object_format").as_deref() != Some("sha1") {
            return Err(Error::Precondition("unsupported repository object format"));
        }
        let grant = field(5).ok_or(Error::Unauthorized)?;
        // administrators do not get implicit access to repository contents
        if flag(&grant, "can_read") != Some(true) {
            return Err(Error::Unauthorized);
        }
        let grants: Vec<alt_repository_service::Grant> =
            alt_json::from_value(grant.get("grants").cloned().unwrap_or(Value::Null))?;
        let mut refs = Refs::parse(row.get::<_, Option<&str>>(7))?;
        let ref_scope = match filter {
            RefFilter::All => RefScope::Complete,
            RefFilter::Bounded if refs.len() <= DEFERRED_REFS => RefScope::Complete,
            RefFilter::Bounded | RefFilter::Nothing => {
                refs = Refs::default();
                RefScope::Deferred
            }
            RefFilter::Prefixes(_) | RefFilter::Tips { .. } => RefScope::Filtered {
                other_tips: row.get(10),
            },
        };
        let ids: Vec<(String, bool)> = alt_json::from_value(field(8).unwrap_or(Value::Null))?;
        Ok(Some(PackView {
            scope: Scope {
                installation_id: self.spec.installation_id.clone(),
                repository_instance_id: repository.into(),
                recovery_epoch: unsigned(
                    number(&repository_row, "recovery_epoch")
                        .ok_or(Error::Integrity("repository epoch"))?,
                )?,
            },
            refs,
            ref_scope,
            default_branch: text(&repository_row, "default_branch")
                .ok_or(Error::Integrity("default branch"))?,
            unreachable: flag(&repository_row, "pack_unreachable") == Some(true),
            led_under: self.led_under(repository),
            writable: flag(&repository_row, "writable") == Some(true)
                && grants
                    .iter()
                    .any(|grant| grant.create || grant.update || grant.delete),
            actor_id: text(&credential_row, "actor_id")
                .ok_or(Error::Integrity("credential actor"))?,
            attribution: alt_json::from_value(attribution)?,
            segments: self.open_segments(repository, &ids)?,
            segment_ids: ids.into_iter().map(|(id, _)| id).collect(),
        }))
    }

    fn pack_view_locked(&mut self, credential: &Credential, repository: &str) -> Result<PackView> {
        let mut tx = crate::reading::transaction(&mut self.client, &self.spec)?;
        // nothing durable is written here; row locks alone need no synchronous flush
        tx.execute("SET LOCAL synchronous_commit=off", &[])?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        // the references are read for a caller who may read them, and for no other
        let mut current = crate::reading::unread(&mut tx, &self.spec, repository)?;
        let auth = crate::store::authorize(&mut tx, &self.spec, credential, &current)?;
        current.refs = crate::reading::refs(&mut tx, repository)?;
        let row = tx.query_one(
            "SELECT default_branch,pack_unreachable FROM alt_repositories WHERE id=$1",
            &[&repository],
        )?;
        let ids = Self::live_segment_ids(&mut tx, repository)?;
        tx.commit()?;
        Ok(PackView {
            scope: current.scope,
            refs: Refs::from(&current.refs),
            ref_scope: RefScope::Complete,
            default_branch: row.get(0),
            unreachable: row.get(1),
            led_under: self.led_under(repository),
            writable: current.writes_enabled
                && auth
                    .grants
                    .iter()
                    .any(|grant| grant.create || grant.update || grant.delete),
            actor_id: auth.actor_id,
            attribution: auth.attribution,
            segments: self.open_segments(repository, &ids)?,
            segment_ids: ids.into_iter().map(|(id, _)| id).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bounded_read_stops_one_row_past_the_deferral_threshold() {
        assert!(VIEW_BOUNDED.contains(&format!(" LIMIT {})", DEFERRED_REFS + 1)));
    }
}
