//! Password sign-in in two steps, so a caller can decide between the password
//! check, which costs one Argon2 computation, and the session write.
use super::{BrowserLogin, audit, binding, login_budget, login_name, user};
use crate::store::{instance, integer, now, token_digest};
use crate::{Error, Result, Store};
use alt_browser_auth::{PublicOrigin, password_hash, password_verify};
use alt_remote_protocol::{ActorKind, Attribution, DeploymentMode};

/// a password that matched; opening the session rechecks the account it matched
pub struct PasswordProof {
    actor: String,
    generation: i64,
    hash: String,
}

fn session_ttl(ttl_ms: u64) -> Result<()> {
    if !(60_000..=43_200_000).contains(&ttl_ms) {
        return Err(Error::Limit("session duration"));
    }
    Ok(())
}

impl Store {
    /// Login rate admission is committed before the expensive password check.
    /// Unknown accounts receive the same bounded work and generic result.
    pub fn verify_browser_password(
        &mut self,
        origin: &PublicOrigin,
        login: &str,
        password: &str,
        network: std::net::IpAddr,
    ) -> Result<Option<PasswordProof>> {
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
        Ok(Some(PasswordProof {
            actor: saved.get("actor_id"),
            generation: saved.get("generation"),
            hash,
        }))
    }
    /// No session is opened for an account that changed after its password matched.
    pub fn open_browser_session(
        &mut self,
        origin: &PublicOrigin,
        proof: PasswordProof,
        ttl_ms: u64,
    ) -> Result<Option<BrowserLogin>> {
        session_ttl(ttl_ms)?;
        let PasswordProof {
            actor,
            generation,
            hash,
        } = proof;
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
    /// both steps, for a caller that waits for the result however long it takes
    pub fn browser_login(
        &mut self,
        origin: &PublicOrigin,
        login: &str,
        password: &str,
        network: std::net::IpAddr,
        ttl_ms: u64,
    ) -> Result<Option<BrowserLogin>> {
        session_ttl(ttl_ms)?;
        match self.verify_browser_password(origin, login, password, network)? {
            Some(proof) => self.open_browser_session(origin, proof, ttl_ms),
            None => Ok(None),
        }
    }
}
