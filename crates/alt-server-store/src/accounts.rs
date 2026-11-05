//! Session ownership and explicit local account recovery.
use crate::browser::{audit_details, binding};
use crate::store::{credential_row, instance, integer, now, require_admin, unsigned};
use crate::{Credential, Error, Result, Store};
use alt_browser_auth::{PublicOrigin, password_hash};
use alt_remote_protocol::DeploymentMode;
use serde::Serialize;

#[derive(Serialize)]
pub struct ActiveBrowserSession {
    /// A database identifier, never an authentication secret.
    pub id: String,
    pub created_at_unix_ms: String,
    pub expires_at_unix_ms: String,
    pub current: bool,
}
impl Store {
    /// Physical recovery also supports pre-browser backups. The actor must
    /// already have local human administrator authority in the restored data;
    /// this never creates a new identity or enables local access in Cloud.
    pub fn recover_local_token(
        &mut self,
        actor: &str,
        token: &str,
        expires: u64,
        reason: &str,
    ) -> Result<()> {
        if self.spec.deployment_mode != DeploymentMode::SelfHosted {
            return Err(Error::Unauthorized);
        }
        if reason.trim().is_empty() || reason.len() > 1024 || reason.chars().any(char::is_control) {
            return Err(Error::Precondition("recovery reason"));
        }
        let mut tx = self.client.transaction()?;
        tx.query_one(
            "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
            &[],
        )?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if expires > now(&mut tx)?.saturating_add(3_600_000) {
            return Err(Error::Precondition(
                "recovery credential maximum lifetime is one hour",
            ));
        }
        let existing: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM alt_credentials WHERE actor_id=$1 AND administrator AND identity_source='self_hosted' AND attribution->>'kind'='human')", &[&actor])?.get(0);
        if !existing {
            return Err(Error::Precondition(
                "existing local human administrator required",
            ));
        }
        let digest = crate::store::token_digest(token);
        let repeated: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM alt_instance_audit WHERE action='credential.operator_recovery' AND subject_id=$1 AND details->>'credential_id'=$2)", &[&actor, &digest])?.get(0);
        crate::store::insert_known_token(
            &mut tx,
            token,
            actor,
            &alt_remote_protocol::Attribution {
                kind: alt_remote_protocol::ActorKind::Human,
                on_behalf_of: None,
                delegation_id: None,
                agent_session: None,
            },
            DeploymentMode::SelfHosted,
            expires,
            true,
        )?;
        if !repeated {
            tx.execute(
                "UPDATE alt_credentials SET revoked=true WHERE actor_id=$1 AND token_digest<>$2",
                &[&actor, &digest],
            )?;
            audit_details(
                &mut tx,
                "local-operator",
                "credential.operator_recovery",
                actor,
                &serde_json::json!({"reason":reason,"credential_id":digest,"old_credentials_revoked":true}),
            )?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    pub fn browser_sessions(
        &mut self,
        credential: &Credential,
        origin: &PublicOrigin,
    ) -> Result<Vec<ActiveBrowserSession>> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        binding(&mut tx, origin)?;
        let actor: String = credential_row(&mut tx, &self.spec, credential)?.get("actor_id");
        if tx
            .query_opt(
                "SELECT 1 FROM alt_web_sessions WHERE token_digest=$1",
                &[&credential.digest],
            )?
            .is_none()
        {
            return Err(Error::Unauthorized);
        }
        let time = integer(now(&mut tx)?)?;
        let rows = tx.query("SELECT s.token_digest,s.created_at,s.expires_at FROM alt_web_sessions s JOIN alt_credentials c ON c.token_digest=s.token_digest JOIN alt_web_accounts a ON a.actor_id=s.actor_id WHERE s.actor_id=$1 AND NOT c.revoked AND s.expires_at>$2 AND s.account_generation=a.generation ORDER BY s.created_at,s.token_digest LIMIT 16", &[&actor,&time])?;
        let mut sessions = Vec::new();
        for row in rows {
            let id: String = row.get(0);
            sessions.push(ActiveBrowserSession {
                current: id == credential.digest,
                id,
                created_at_unix_ms: unsigned(row.get(1))?.to_string(),
                expires_at_unix_ms: unsigned(row.get(2))?.to_string(),
            });
        }
        tx.commit()?;
        Ok(sessions)
    }
    pub fn revoke_browser_session(
        &mut self,
        credential: &Credential,
        origin: &PublicOrigin,
        id: &str,
    ) -> Result<()> {
        alt_task::check_id(id).map_err(|_| Error::Unauthorized)?;
        let mut tx = self.client.transaction()?;
        tx.query_one(
            "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
            &[],
        )?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        binding(&mut tx, origin)?;
        let actor: String = credential_row(&mut tx, &self.spec, credential)?.get("actor_id");
        if tx
            .query_opt(
                "SELECT 1 FROM alt_web_sessions WHERE token_digest=$1",
                &[&credential.digest],
            )?
            .is_none()
        {
            return Err(Error::Unauthorized);
        }
        if tx
            .query_opt(
                "SELECT 1 FROM alt_web_sessions WHERE token_digest=$1 AND actor_id=$2",
                &[&id, &actor],
            )?
            .is_none()
        {
            return Err(Error::Unauthorized);
        }
        tx.execute(
            "UPDATE alt_credentials SET revoked=true WHERE token_digest=$1",
            &[&id],
        )?;
        audit_details(
            &mut tx,
            &actor,
            "session.revoke",
            id,
            &serde_json::json!({}),
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    /// Explicit administrative reset revokes every existing credential for the
    /// selected actor. An identity with the same email is never inferred.
    pub fn reset_local_password(
        &mut self,
        admin: &Credential,
        actor: &str,
        password: &str,
    ) -> Result<()> {
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
        tx.query_one(
            "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
            &[],
        )?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        let owner: String = credential_row(&mut tx, &self.spec, admin)?.get("actor_id");
        if tx.execute("UPDATE alt_web_accounts SET password_hash=$2,generation=generation+1 WHERE actor_id=$1", &[&actor,&hash])?!=1{return Err(Error::Precondition("unknown local account"));}
        tx.execute(
            "UPDATE alt_credentials SET revoked=true WHERE actor_id=$1",
            &[&actor],
        )?;
        audit_details(
            &mut tx,
            &owner,
            "user.password_reset",
            actor,
            &serde_json::json!({"credentials_revoked":true}),
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    /// Physical operator recovery: requires the configured receipt key and DB
    /// access, works only for an existing self-hosted local administrator, and
    /// never becomes an HTTP/bootstrap fallback.
    pub fn recover_local_administrator(
        &mut self,
        actor: &str,
        password: &str,
        reason: &str,
    ) -> Result<()> {
        if self.spec.deployment_mode != DeploymentMode::SelfHosted {
            return Err(Error::Unauthorized);
        }
        if reason.trim().is_empty() || reason.len() > 1024 || reason.chars().any(char::is_control) {
            return Err(Error::Precondition("recovery reason"));
        }
        let hash = password_hash(password).map_err(|_| Error::Precondition("password policy"))?;
        let mut tx = self.client.transaction()?;
        tx.query_one(
            "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
            &[],
        )?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if tx.execute("UPDATE alt_web_accounts SET password_hash=$2,disabled=false,generation=generation+1 WHERE actor_id=$1 AND administrator", &[&actor,&hash])?!=1{return Err(Error::Precondition("existing local administrator required"));}
        tx.execute(
            "UPDATE alt_credentials SET revoked=true WHERE actor_id=$1",
            &[&actor],
        )?;
        audit_details(
            &mut tx,
            "local-operator",
            "user.administrator_recovery",
            actor,
            &serde_json::json!({"reason":reason,"credentials_revoked":true}),
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}
