//! Local browser identities and sessions. They share the same credential and
//! repository authorization checks as CLI/Git. Cloud cannot create local users.
use crate::database::Transaction;
use crate::store::{credential_row, instance, integer, now, require_admin, token_digest, unsigned};
use crate::{Credential, Error, Result, Store};
use alt_browser_auth::{PublicOrigin, password_hash, password_verify};
use alt_remote_protocol::{ActorKind, Attribution, DeploymentMode};
use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct LocalUser {
    pub actor_id: String,
    pub login_name: String,
    pub display_name: String,
    pub administrator: bool,
    pub disabled: bool,
    pub generation: u64,
}
pub struct BrowserLogin {
    pub token: String,
    pub expires_at_unix_ms: u64,
    pub user: LocalUser,
}
#[derive(Serialize)]
pub struct BrowserSession {
    pub user: LocalUser,
    pub expires_at_unix_ms: u64,
}

fn login_name(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b))
    {
        return Err(Error::Precondition("login name"));
    }
    Ok(value.to_ascii_lowercase())
}
fn user(row: &postgres::Row) -> Result<LocalUser> {
    Ok(LocalUser {
        actor_id: row.get("actor_id"),
        login_name: row.get("login_name"),
        display_name: row.get("display_name"),
        administrator: row.get("administrator"),
        disabled: row.get("disabled"),
        generation: unsigned(row.get("generation"))?,
    })
}
pub(crate) fn audit(
    tx: &mut Transaction<'_>,
    actor: &str,
    action: &str,
    subject: &str,
) -> Result<()> {
    audit_details(tx, actor, action, subject, &serde_json::json!({}))
}
pub(crate) fn audit_details(
    tx: &mut Transaction<'_>,
    actor: &str,
    action: &str,
    subject: &str,
    details: &serde_json::Value,
) -> Result<()> {
    let time = integer(now(tx)?)?;
    tx.execute("INSERT INTO alt_instance_audit(actor_id,action,subject_id,details,occurred_at) VALUES($1,$2,$3,$4,$5)", &[&actor,&action,&subject,&details,&time])?;
    Ok(())
}
pub(crate) fn audit_admin(
    tx: &mut Transaction<'_>,
    spec: &crate::InstanceSpec,
    admin: &Credential,
    action: &str,
    subject: &str,
    details: serde_json::Value,
) -> Result<()> {
    let actor: String = credential_row(tx, spec, admin)?.get("actor_id");
    audit_details(tx, &actor, action, subject, &details)
}

pub(crate) fn binding(tx: &mut Transaction<'_>, origin: &PublicOrigin) -> Result<()> {
    let saved = tx.query_opt(
        "SELECT public_origin FROM alt_browser_binding WHERE singleton FOR SHARE",
        &[],
    )?;
    if saved.is_none_or(|row| row.get::<_, String>(0) != origin.as_str()) {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
impl Store {
    /// Operator action. An existing origin is immutable without an explicit
    /// instance migration; deployment cannot silently retarget browser sessions.
    pub fn enable_browser(&mut self, admin: &Credential, origin: &PublicOrigin) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        tx.execute("INSERT INTO alt_browser_binding(singleton,public_origin) VALUES(true,$1) ON CONFLICT(singleton) DO NOTHING", &[&origin.as_str()])?;
        binding(&mut tx, origin)?;
        let actor: String = credential_row(&mut tx, &self.spec, admin)?.get("actor_id");
        audit(&mut tx, &actor, "browser.enable", origin.as_str())?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    pub fn check_browser_binding(&mut self, origin: Option<&PublicOrigin>) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        match origin {
            Some(origin) => binding(&mut tx, origin)?,
            None => {
                if tx
                    .query_opt("SELECT 1 FROM alt_browser_binding WHERE singleton", &[])?
                    .is_some()
                {
                    return Err(Error::Precondition("browser configuration required"));
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn local_users(
        &mut self,
        admin: &Credential,
        after: &str,
        limit: usize,
    ) -> Result<Vec<LocalUser>> {
        if limit == 0 || limit > 100 || after.len() > 128 {
            return Err(Error::Limit("user page"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        let rows = tx.query("SELECT actor_id,login_name,display_name,administrator,disabled,generation FROM alt_web_accounts WHERE actor_id>$1 ORDER BY actor_id LIMIT $2", &[&after,&integer(limit as u64)?])?;
        let values = rows.iter().map(user).collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(values)
    }
    pub fn create_local_user(
        &mut self,
        admin: &Credential,
        actor: &str,
        login: &str,
        display_name: &str,
        password: &str,
        administrator: bool,
    ) -> Result<LocalUser> {
        let login = login_name(login)?;
        alt_remote_protocol::Scope {
            installation_id: actor.into(),
            repository_instance_id: "actor".into(),
            recovery_epoch: 1,
        }
        .validate()?;
        if display_name.trim().is_empty()
            || display_name.len() > 256
            || display_name.chars().any(char::is_control)
        {
            return Err(Error::Precondition("display name"));
        }
        {
            let mut tx = self.client.transaction()?;
            instance(&mut tx, &self.spec, &self.signing_key)?;
            require_admin(&mut tx, &self.spec, admin)?;
            if self.spec.deployment_mode != DeploymentMode::SelfHosted {
                return Err(Error::Unauthorized);
            }
            tx.commit()?;
        }
        let hash = password_hash(password).map_err(|_| Error::Precondition("password policy"))?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        if tx.execute("INSERT INTO alt_web_accounts(actor_id,login_name,display_name,password_hash,administrator) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING", &[&actor,&login,&display_name,&hash,&administrator])? != 1 {
            return Err(Error::Precondition("user or login name already exists"));
        }
        let owner: String = credential_row(&mut tx, &self.spec, admin)?.get("actor_id");
        audit_details(
            &mut tx,
            &owner,
            "user.create",
            actor,
            &serde_json::json!({"login_name":login,"display_name":display_name,"administrator":administrator}),
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(LocalUser {
            actor_id: actor.into(),
            login_name: login,
            display_name: display_name.into(),
            administrator,
            disabled: false,
            generation: 1,
        })
    }
    /// Login rate admission is committed before the expensive password check.
    /// Unknown accounts receive the same bounded work and generic result.
    pub fn browser_login(
        &mut self,
        origin: &PublicOrigin,
        login: &str,
        password: &str,
        network: std::net::IpAddr,
        ttl_ms: u64,
    ) -> Result<Option<BrowserLogin>> {
        if !(60_000..=43_200_000).contains(&ttl_ms) {
            return Err(Error::Limit("session duration"));
        }
        if password.len() > 1024 {
            return Ok(None);
        }
        let login = login_name(login).map_err(|_| Error::Unauthorized)?;
        let saved = {
            let mut tx = self.client.transaction()?;
            instance(&mut tx, &self.spec, &self.signing_key)?;
            if self.spec.deployment_mode != DeploymentMode::SelfHosted {
                return Err(Error::Unauthorized);
            }
            binding(&mut tx, origin)?;
            login_budget(&mut tx, &login, network)?;
            let saved=tx.query_opt("SELECT actor_id,password_hash,generation FROM alt_web_accounts WHERE login_name=$1 AND NOT disabled", &[&login])?;
            tx.commit()?;
            saved
        };
        let Some(saved) = saved else {
            // A fresh, valid input incurs one Argon2 computation without creating a user.
            let padding = format!("unknown-login:{password}");
            let _ = password_hash(&padding.chars().take(256).collect::<String>())
                .map_err(|_| Error::Integrity("password operation"))?;
            return Ok(None);
        };
        let hash: String = saved.get("password_hash");
        if !password_verify(password, &hash).map_err(|_| Error::Integrity("password encoding"))? {
            return Ok(None);
        }
        let actor: String = saved.get("actor_id");
        let generation: i64 = saved.get("generation");
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        binding(&mut tx, origin)?;
        let row = tx.query_one(
            "SELECT * FROM alt_web_accounts WHERE actor_id=$1 FOR UPDATE",
            &[&actor],
        )?;
        if row.get::<_, bool>("disabled")
            || row.get::<_, i64>("generation") != generation
            || row.get::<_, String>("password_hash") != hash
        {
            return Ok(None);
        }
        let time = now(&mut tx)?;
        let count:i64=tx.query_one("SELECT count(*) FROM alt_web_sessions s JOIN alt_credentials c ON c.token_digest=s.token_digest WHERE s.actor_id=$1 AND s.expires_at>$2 AND s.account_generation=$3 AND NOT c.revoked", &[&actor,&integer(time)?,&generation])?.get(0);
        if count >= 16 {
            return Err(Error::Precondition(
                "active session limit; revoke an existing session",
            ));
        }
        let expires = time
            .checked_add(ttl_ms)
            .ok_or(Error::Limit("session expiry"))?;
        let token = alt_browser_auth::token();
        let attribution = Attribution {
            kind: ActorKind::Human,
            on_behalf_of: None,
            delegation_id: None,
            agent_session: None,
        };
        crate::store::insert_known_token(
            &mut tx,
            &token,
            &actor,
            &attribution,
            DeploymentMode::SelfHosted,
            expires,
            row.get("administrator"),
        )?;
        tx.execute("INSERT INTO alt_web_sessions(token_digest,actor_id,account_generation,expires_at,created_at) VALUES($1,$2,$3,$4,$5)", &[&token_digest(&token),&actor,&generation,&integer(expires)?,&integer(time)?])?;
        audit(&mut tx, &actor, "session.login", &actor)?;
        let account = user(&row)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(Some(BrowserLogin {
            token,
            expires_at_unix_ms: expires,
            user: account,
        }))
    }
    pub fn browser_session(
        &mut self,
        credential: &Credential,
        origin: &PublicOrigin,
    ) -> Result<BrowserSession> {
        if self.spec.deployment_mode == DeploymentMode::Cloud {
            return self.cloud_browser_session(credential, origin);
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        binding(&mut tx, origin)?;
        credential_row(&mut tx, &self.spec, credential)?;
        let row=tx.query_opt("SELECT a.*,s.expires_at FROM alt_web_sessions s JOIN alt_web_accounts a ON a.actor_id=s.actor_id WHERE s.token_digest=$1", &[&credential.digest])?.ok_or(Error::Unauthorized)?;
        let value = BrowserSession {
            user: user(&row)?,
            expires_at_unix_ms: unsigned(row.get("expires_at"))?,
        };
        tx.commit()?;
        Ok(value)
    }
    pub fn browser_logout(&mut self, credential: &Credential, origin: &PublicOrigin) -> Result<()> {
        if self.spec.deployment_mode == DeploymentMode::Cloud {
            return self.revoke_cloud_browser_session(
                credential,
                origin,
                &credential.digest.clone(),
            );
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        binding(&mut tx, origin)?;
        tx.query_opt(
            "SELECT token_digest FROM alt_credentials WHERE token_digest=$1 FOR UPDATE",
            &[&credential.digest],
        )?
        .ok_or(Error::Unauthorized)?;
        let row = credential_row(&mut tx, &self.spec, credential)?;
        if tx
            .query_opt(
                "SELECT 1 FROM alt_web_sessions WHERE token_digest=$1",
                &[&credential.digest],
            )?
            .is_none()
        {
            return Err(Error::Unauthorized);
        }
        tx.execute(
            "UPDATE alt_credentials SET revoked=true WHERE token_digest=$1",
            &[&credential.digest],
        )?;
        let actor: String = row.get("actor_id");
        audit(&mut tx, &actor, "session.logout", &actor)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    pub fn disable_local_user(
        &mut self,
        admin: &Credential,
        actor: &str,
        disabled: bool,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        // Account revocation updates both accounts and credentials. Acquire the
        // instance barrier before either row family; normal requests hold its
        // shared lock before credential/account locks, so no inverse lock order
        // can deadlock an active request against revocation.
        tx.query_one(
            "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
            &[],
        )?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        if self.spec.deployment_mode != DeploymentMode::SelfHosted {
            return Err(Error::Unauthorized);
        }
        let owner: String = credential_row(&mut tx, &self.spec, admin)?.get("actor_id");
        if disabled && owner == actor {
            return Err(Error::Precondition(
                "cannot disable the active administrator",
            ));
        }
        if tx.execute(
            "UPDATE alt_web_accounts SET disabled=$2,generation=generation+1 WHERE actor_id=$1",
            &[&actor, &disabled],
        )? != 1
        {
            return Err(Error::Precondition("unknown user"));
        }
        tx.execute(
            "UPDATE alt_credentials SET revoked=true WHERE actor_id=$1",
            &[&actor],
        )?;
        audit(
            &mut tx,
            &owner,
            if disabled {
                "user.disable"
            } else {
                "user.enable"
            },
            actor,
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}

/// Called by every credential read inside its authorizing transaction. Existing
/// API tokens retain their explicit capabilities, while disabling the account
/// or advancing a browser session's account generation takes effect immediately.
pub(crate) fn check_credential(
    tx: &mut Transaction<'_>,
    credential: &Credential,
    actor: &str,
) -> Result<()> {
    if let Some(row) = tx.query_opt(
        "SELECT disabled FROM alt_web_accounts WHERE actor_id=$1 FOR SHARE",
        &[&actor],
    )? && row.get::<_, bool>(0)
    {
        return Err(Error::Unauthorized);
    }
    let time = integer(now(tx)?)?;
    if let Some(row)=tx.query_opt("SELECT s.account_generation,a.generation,s.expires_at FROM alt_web_sessions s JOIN alt_web_accounts a ON a.actor_id=s.actor_id WHERE s.token_digest=$1 FOR SHARE OF s,a", &[&credential.digest])? && (row.get::<_,i64>(0)!=row.get::<_,i64>(1)||row.get::<_,i64>(2)<=time) {return Err(Error::Unauthorized);}
    Ok(())
}
pub(crate) fn login_budget(
    tx: &mut Transaction<'_>,
    login: &str,
    network: std::net::IpAddr,
) -> Result<()> {
    let time = integer(now(tx)?)?;
    // A single stable global row serializes admission and bounds the number of
    // attacker-chosen keys written in each minute. Stale keys are removed here.
    let rows = [
        ("global".to_owned(), 1000),
        (
            format!("network:{}", alt_browser_auth::digest(&network.to_string())),
            100,
        ),
        (format!("login:{}", alt_browser_auth::digest(login)), 10),
    ];
    for (key, max) in rows {
        let row=tx.query_one("INSERT INTO alt_login_budgets(key,window_start,attempts) VALUES($1,$2,1) ON CONFLICT(key) DO UPDATE SET window_start=CASE WHEN alt_login_budgets.window_start<=$2-60000 THEN $2 ELSE alt_login_budgets.window_start END,attempts=CASE WHEN alt_login_budgets.window_start<=$2-60000 THEN 1 ELSE alt_login_budgets.attempts+1 END RETURNING attempts", &[&key,&time])?;
        if row.get::<_, i32>(0) > max {
            return Err(Error::Precondition(
                "login admission limit; retry after one minute",
            ));
        }
    }
    tx.execute(
        "DELETE FROM alt_login_budgets WHERE key<>'global' AND window_start<$1::bigint-60000",
        &[&time],
    )?;
    Ok(())
}
