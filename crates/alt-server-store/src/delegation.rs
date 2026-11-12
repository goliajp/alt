//! Agent capabilities always remain attached to a live human credential.
use crate::Jsonb;
use crate::database::Transaction;
use crate::store::{
    authorize, credential_row, instance, integer, now, snapshot, token_digest, unsigned,
};
use crate::{Credential, Error, InstanceSpec, Result, Store};
use alt_remote_protocol::{
    ActorKind, Attribution,
    authorization::{Grant, RefSelector},
    delegation::{AgentScope, DelegationInfo, DelegationRequest, MAX_LIFETIME_MS},
};
use alt_repository_service::Authorization;

fn contains_ref(parent: &RefSelector, child: &RefSelector) -> bool {
    match (parent, child) {
        (RefSelector::Exact(a), RefSelector::Exact(b)) => a == b,
        (RefSelector::Namespace(a), RefSelector::Exact(b) | RefSelector::Namespace(b)) => {
            b.starts_with(a)
        }
        _ => false,
    }
}
fn contains_grant(parent: &Grant, child: &Grant) -> bool {
    contains_ref(&parent.refs, &child.refs)
        && (!child.create || parent.create)
        && (!child.update || parent.update)
        && (!child.delete || parent.delete)
        && (!child.force || parent.force)
        && match (&parent.paths, &child.paths) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(parent), Some(child)) => child.iter().all(|c| {
                parent
                    .iter()
                    .any(|p| c == p || (c.starts_with(p) && c.get(p.len()) == Some(&b'/')))
            }),
        }
}
pub(crate) fn parent(tx: &mut Transaction<'_>, digest: &str) -> Result<Option<String>> {
    Ok(tx
        .query_opt(
            "SELECT parent_digest FROM alt_delegations WHERE token_digest=$1",
            &[&digest],
        )?
        .map(|row| row.get(0)))
}
pub(crate) fn check(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
    row: &alt_pg::Row,
) -> Result<bool> {
    let Some(parent) = parent(tx, &credential.digest)? else {
        let attribution: Attribution = row.get::<_, Jsonb>("attribution").read()?;
        if attribution.kind == ActorKind::Agent {
            return Err(Error::Unauthorized);
        }
        return Ok(false);
    };
    // No delegation chains: validate before recursion, including corrupted rows.
    let parent_kind = tx
        .query_opt(
            "SELECT attribution FROM alt_credentials WHERE token_digest=$1",
            &[&parent],
        )?
        .ok_or(Error::Unauthorized)?;
    let parent_kind: Attribution = parent_kind.get::<_, Jsonb>(0).read()?;
    if parent_kind.kind != ActorKind::Human {
        return Err(Error::Unauthorized);
    }
    let parent = credential_row(tx, spec, &Credential { digest: parent })?;
    let attribution: Attribution = row.get::<_, Jsonb>("attribution").read()?;
    if parent.get::<_, String>("actor_id") != row.get::<_, String>("actor_id")
        || row.get::<_, bool>("administrator")
        || attribution.kind != ActorKind::Agent
        || attribution.on_behalf_of.as_deref() != Some(parent.get::<_, String>("actor_id").as_str())
    {
        return Err(Error::Unauthorized);
    }
    Ok(true)
}
pub(crate) fn restrict(
    tx: &mut Transaction<'_>,
    credential: &Credential,
    auth: &mut Authorization,
) -> Result<()> {
    let Some(row) = tx.query_opt(
        "SELECT scope FROM alt_delegations WHERE token_digest=$1",
        &[&credential.digest],
    )?
    else {
        return Ok(());
    };
    let scope: AgentScope = row.get::<_, Jsonb>(0).read()?;
    if scope.repository != auth.scope {
        return Err(Error::Unauthorized);
    }
    // A narrowed parent policy cannot be overridden by the original child
    // grant. Each retained capability must still fit one current parent grant.
    auth.grants = scope
        .grants
        .into_iter()
        .filter(|child| {
            auth.grants
                .iter()
                .any(|parent| contains_grant(parent, child))
        })
        .collect();
    auth.publish_records &= scope.publish_records;
    Ok(())
}
fn human(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
) -> Result<alt_pg::Row> {
    let row = credential_row(tx, spec, credential)?;
    let attribution: Attribution = row.get::<_, Jsonb>("attribution").read()?;
    if attribution.kind != ActorKind::Human {
        return Err(Error::Unauthorized);
    }
    Ok(row)
}
fn info(row: &alt_pg::Row) -> Result<DelegationInfo> {
    Ok(DelegationInfo {
        credential_id: row.get("token_digest"),
        attribution: row.get::<_, Jsonb>("attribution").read()?,
        scope: row.get::<_, Jsonb>("scope").read()?,
        purpose: row.get("purpose"),
        created_at_unix_ms: unsigned(row.get("created_at"))?,
        expires_at_unix_ms: unsigned(row.get("expires_at"))?,
        revoked: row.get("revoked"),
    })
}
impl Store {
    /// The client persists its private token and immutable request before calling
    /// this method. Exact retries return the same record and never revive it.
    pub fn issue_agent(
        &mut self,
        parent: &Credential,
        request: &DelegationRequest,
    ) -> Result<DelegationInfo> {
        request.validate()?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        tx.batch_execute("LOCK TABLE alt_delegations IN SHARE ROW EXCLUSIVE MODE")?;
        let row = human(&mut tx, &self.spec, parent)?;
        let actor: String = row.get("actor_id");
        let time = now(&mut tx)?;
        let expires = request.expires_at_unix_ms;
        if request.attribution.on_behalf_of.as_deref() != Some(&actor)
            || expires > unsigned(row.get("expires_at"))?
            || expires > time.saturating_add(MAX_LIFETIME_MS)
            || expires <= time
        {
            return Err(Error::Unauthorized);
        }
        let current = snapshot(
            &mut tx,
            &self.spec,
            &request.scope.repository.repository_instance_id,
        )?;
        if current.scope != request.scope.repository {
            return Err(Error::Unauthorized);
        }
        let auth = authorize(&mut tx, &self.spec, parent, &current)?;
        if (request.scope.publish_records && !auth.publish_records)
            || request.scope.grants.iter().any(|child| {
                !auth
                    .grants
                    .iter()
                    .any(|parent| contains_grant(parent, child))
            })
        {
            return Err(Error::Unauthorized);
        }
        let digest = token_digest(&request.access_token);
        let existing = tx.query_opt(
            "SELECT parent_digest,scope,purpose FROM alt_delegations WHERE token_digest=$1",
            &[&digest],
        )?;
        let value = Jsonb::from(&request.scope)?;
        if let Some(saved) = &existing {
            if saved.get::<_, String>(0) != parent.digest
                || saved.get::<_, Jsonb>(1) != value
                || saved.get::<_, String>(2) != request.purpose
            {
                return Err(Error::Precondition("agent credential replay differs"));
            }
        } else {
            // An unrelated existing credential cannot be converted into a
            // delegation, even if its raw token and actor happen to match.
            if tx
                .query_opt(
                    "SELECT 1 FROM alt_credentials WHERE token_digest=$1",
                    &[&digest],
                )?
                .is_some()
            {
                return Err(Error::Precondition("credential already exists"));
            }
            let count:i64 = tx.query_one("SELECT count(*) FROM alt_delegations d JOIN alt_credentials c USING(token_digest) WHERE c.actor_id=$1 AND NOT c.revoked AND c.expires_at>$2", &[&actor,&integer(time)?])?.get(0);
            if count >= 64 {
                return Err(Error::Limit("active agent credentials"));
            }
            if tx
                .query_opt(
                    "SELECT 1 FROM alt_delegations WHERE parent_digest=$1 AND delegation_id=$2",
                    &[&parent.digest, &request.attribution.delegation_id],
                )?
                .is_some()
            {
                return Err(Error::Precondition("delegation id already exists"));
            }
        }
        crate::store::insert_known_token(
            &mut tx,
            &request.access_token,
            &actor,
            &request.attribution,
            self.spec.deployment_mode,
            expires,
            false,
        )?;
        if existing.is_none() {
            tx.execute("INSERT INTO alt_delegations(token_digest,parent_digest,scope,purpose,delegation_id,created_at) VALUES($1,$2,$3,$4,$5,$6)", &[&digest,&parent.digest,&value,&request.purpose,&request.attribution.delegation_id,&integer(time)?])?;
            crate::browser::audit_details(
                &mut tx,
                &actor,
                "agent.issue",
                &digest,
                &Jsonb(
                    alt_json::json!({"parent":parent.digest,"attribution":request.attribution,"scope":request.scope,"purpose":request.purpose,"expires_at":expires.to_string()}),
                ),
            )?;
        }
        let result = info(&tx.query_one("SELECT c.*,d.scope,d.purpose,d.created_at FROM alt_credentials c JOIN alt_delegations d USING(token_digest) WHERE token_digest=$1", &[&digest])?)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
    /// Human owners can inspect/revoke their agents from another current login.
    /// Administrative authority alone never grants access to another owner.
    pub fn agent_delegation(&mut self, owner: &Credential, digest: &str) -> Result<DelegationInfo> {
        crate::bytes::check_digest(digest)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let actor: String = human(&mut tx, &self.spec, owner)?.get("actor_id");
        let row = tx.query_opt("SELECT c.*,d.scope,d.purpose,d.created_at FROM alt_credentials c JOIN alt_delegations d USING(token_digest) WHERE token_digest=$1 AND actor_id=$2", &[&digest,&actor])?.ok_or(Error::Unauthorized)?;
        let result = info(&row)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn agent_delegations(
        &mut self,
        owner: &Credential,
        after: &str,
    ) -> Result<crate::Page<DelegationInfo>> {
        if !after.is_empty() {
            crate::bytes::check_digest(after)?;
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let actor: String = human(&mut tx, &self.spec, owner)?.get("actor_id");
        let rows = tx.query("SELECT c.*,d.scope,d.purpose,d.created_at FROM alt_credentials c JOIN alt_delegations d USING(token_digest) WHERE actor_id=$1 AND token_digest>$2 ORDER BY token_digest LIMIT 101", &[&actor,&after])?;
        let more = rows.len() > 100;
        let items = rows
            .iter()
            .take(100)
            .map(info)
            .collect::<Result<Vec<_>>>()?;
        let next = if more {
            items.last().map(|item| item.credential_id.clone())
        } else {
            None
        };
        tx.commit()?;
        Ok(crate::Page { items, next })
    }
    pub fn revoke_agent(&mut self, owner: &Credential, digest: &str) -> Result<()> {
        crate::bytes::check_digest(digest)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let actor: String = human(&mut tx, &self.spec, owner)?.get("actor_id");
        if tx.query_opt("SELECT 1 FROM alt_delegations d JOIN alt_credentials c USING(token_digest) WHERE token_digest=$1 AND actor_id=$2", &[&digest,&actor])?.is_none() { return Err(Error::Unauthorized); }
        if tx.execute(
            "UPDATE alt_credentials SET revoked=true WHERE token_digest=$1 AND NOT revoked",
            &[&digest],
        )? == 1
        {
            crate::browser::audit(&mut tx, &actor, "agent.revoke", digest)?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}
