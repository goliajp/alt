//! Durable device consent and credential issuance. Human CLI credentials use the
//! same current account checks as browser sessions; agent delegation is separate.
use crate::database::Transaction;
use crate::store::{credential_row, instance, integer, now, token_digest, unsigned};
use crate::{Credential, Error, Result, Store};
use alt_browser_auth::PublicOrigin;
use alt_remote_protocol::{ActorKind, Attribution, DeploymentMode, login::*};
use postgres::Row;
use serde::Serialize;

#[derive(Serialize, alt_json::ToJson)]
pub struct DeviceConsent {
    pub id: String,
    pub client_id: &'static str,
    pub scope: &'static str,
    pub state: String,
    pub expires_at_unix_ms: u64,
}
pub enum DevicePoll {
    Waiting(DeviceError),
    Ready { expires_at_unix_ms: u64 },
}
#[derive(Serialize, alt_json::ToJson)]
pub struct ClientCredential {
    pub id: String,
    pub client_id: String,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub current: bool,
}
fn human(row: &Row) -> Result<()> {
    let attribution: Attribution = serde_json::from_value(row.get("attribution"))?;
    if attribution.kind != ActorKind::Human {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
fn browser(tx: &mut Transaction<'_>, mode: DeploymentMode, credential: &Credential) -> Result<()> {
    let found: bool = match mode {
        DeploymentMode::Cloud => tx.query_one("SELECT EXISTS(SELECT 1 FROM alt_cloud_credentials WHERE token_digest=$1 AND browser)", &[&credential.digest])?.get(0),
        DeploymentMode::SelfHosted => tx.query_one("SELECT EXISTS(SELECT 1 FROM alt_web_sessions WHERE token_digest=$1)", &[&credential.digest])?.get(0),
    };
    if !found {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
fn info(row: &Row) -> Result<DeviceConsent> {
    Ok(DeviceConsent {
        id: row.get("device_digest"),
        client_id: CLI_CLIENT_ID,
        scope: CLI_SCOPE,
        state: row.get("state"),
        expires_at_unix_ms: unsigned(row.get("expires_at"))?,
    })
}
fn code_digest(code: &str) -> Result<String> {
    Ok(alt_browser_auth::digest(&user_code(code)?))
}
impl Store {
    /// Possession-based revocation of the public client's own credential.
    /// Unknown/already revoked tokens succeed without an oracle; stopping an
    /// existing credential does not depend on control-plane availability.
    pub fn revoke_client_token(&mut self, token: &str) -> Result<()> {
        crate::store::validate_token(token)?;
        let digest = token_digest(token);
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let row=tx.query_opt("UPDATE alt_credentials c SET revoked=true WHERE c.token_digest=$1 AND NOT c.revoked AND (EXISTS(SELECT 1 FROM alt_client_credentials m WHERE m.token_digest=c.token_digest) OR EXISTS(SELECT 1 FROM alt_delegations d WHERE d.token_digest=c.token_digest)) RETURNING c.actor_id", &[&digest])?;
        if let Some(row) = row {
            crate::browser::audit(&mut tx, &row.get::<_, String>(0), "client.revoke", &digest)?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    pub fn begin_device_authorization(
        &mut self,
        origin: &PublicOrigin,
        device_digest: &str,
        user_code: &str,
        credential_digest: &str,
        network: std::net::IpAddr,
    ) -> Result<u64> {
        crate::bytes::check_digest(device_digest)?;
        crate::bytes::check_digest(credential_digest)?;
        let user_digest = code_digest(user_code)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        crate::browser::binding(&mut tx, origin)?;
        let admission = crate::browser::login_budget(&mut tx, device_digest, network);
        if admission.is_err() {
            tx.commit()?;
            return admission.map(|()| 0);
        }
        let time = now(&mut tx)?;
        let expires = time.saturating_add(DEVICE_LIFETIME_MS);
        tx.execute(
            "DELETE FROM alt_device_authorizations WHERE expires_at<=$1",
            &[&integer(time)?],
        )?;
        tx.execute("INSERT INTO alt_device_authorizations(device_digest,user_digest,credential_digest,state,created_at,expires_at,next_poll_at,interval_ms) VALUES($1,$2,$3,'pending',$4,$5,$6,$7)", &[&device_digest,&user_digest,&credential_digest,&integer(time)?,&integer(expires)?,&integer(time.saturating_add(DEVICE_INTERVAL_MS))?,&integer(DEVICE_INTERVAL_MS)?])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(expires)
    }
    /// Lookup admission is tied to the authenticated actor, not to the guessed
    /// code. Failed guesses commit their budget before returning a generic error.
    pub fn device_consent(
        &mut self,
        credential: &Credential,
        origin: &PublicOrigin,
        code: &str,
        network: std::net::IpAddr,
    ) -> Result<DeviceConsent> {
        let digest = code_digest(code)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        crate::browser::binding(&mut tx, origin)?;
        let parent = credential_row(&mut tx, &self.spec, credential)?;
        human(&parent)?;
        browser(&mut tx, self.spec.deployment_mode, credential)?;
        let actor: String = parent.get("actor_id");
        let admission =
            crate::browser::login_budget(&mut tx, &format!("device-consent:{actor}"), network);
        if admission.is_err() {
            tx.commit()?;
            return admission.and(Err(Error::Unauthorized));
        }
        let time = integer(now(&mut tx)?)?;
        let row = tx.query_opt(
            "SELECT * FROM alt_device_authorizations WHERE user_digest=$1 AND expires_at>$2",
            &[&digest, &time],
        )?;
        tx.commit()?;
        info(&row.ok_or(Error::Unauthorized)?)
    }
    /// A current browser principal approves or denies one code. The HTTP owner
    /// re-encrypts a Cloud grant for the new token; neither code nor token enters
    /// the database as a bearer secret. Approval and issuance commit together.
    #[allow(clippy::too_many_arguments)]
    pub fn decide_device_authorization(
        &mut self,
        credential: &Credential,
        origin: &PublicOrigin,
        code: &str,
        allow: bool,
        token: &str,
        sealed_cloud_grant: Option<&str>,
    ) -> Result<DeviceConsent> {
        crate::store::validate_token(token)?;
        let digest = code_digest(code)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        crate::browser::binding(&mut tx, origin)?;
        // Consent is infrequent. Serialize issuance before credential locks so
        // the active-credential bound also holds across concurrent approvals.
        tx.batch_execute("LOCK TABLE alt_client_credentials IN SHARE ROW EXCLUSIVE MODE")?;
        let parent = credential_row(&mut tx, &self.spec, credential)?;
        human(&parent)?;
        browser(&mut tx, self.spec.deployment_mode, credential)?;
        let row = tx
            .query_opt(
                "SELECT * FROM alt_device_authorizations WHERE user_digest=$1 FOR UPDATE",
                &[&digest],
            )?
            .ok_or(Error::Unauthorized)?;
        let time = now(&mut tx)?;
        if unsigned(row.get("expires_at"))? <= time
            || row.get::<_, String>("credential_digest") != token_digest(token)
        {
            return Err(Error::Unauthorized);
        }
        let next = if allow { "approved" } else { "denied" };
        if row.get::<_, String>("state") != "pending" {
            if row.get::<_, String>("state") == next
                && row.get::<_, Option<String>>("approved_by").as_deref()
                    == Some(&credential.digest)
            {
                return info(&row);
            }
            return Err(Error::Unauthorized);
        }
        let actor: String = parent.get("actor_id");
        let device: String = row.get("device_digest");
        if allow {
            let count: i64 = tx.query_one("SELECT count(*) FROM alt_credentials c JOIN alt_client_credentials m USING(token_digest) WHERE c.actor_id=$1 AND NOT c.revoked AND c.expires_at>$2", &[&actor,&integer(time)?])?.get(0);
            if count >= 64 {
                return Err(Error::Precondition(
                    "active CLI credential limit; revoke an existing credential",
                ));
            }
            let expires =
                unsigned(parent.get("expires_at"))?.min(time.saturating_add(8 * 3_600_000));
            let attribution: Attribution = serde_json::from_value(parent.get("attribution"))?;
            crate::store::insert_known_token(
                &mut tx,
                token,
                &actor,
                &attribution,
                self.spec.deployment_mode,
                expires,
                parent.get("administrator"),
            )?;
            if self.spec.deployment_mode == DeploymentMode::Cloud {
                let sealed = sealed_cloud_grant.ok_or(Error::Unauthorized)?;
                crate::cloud::ciphertext(sealed)?;
                tx.execute("INSERT INTO alt_cloud_credentials(token_digest,claims,sealed_grant,created_at,browser) SELECT $1,claims,$2,$3,false FROM alt_cloud_credentials WHERE token_digest=$4 AND browser", &[&token_digest(token),&sealed,&integer(time)?,&credential.digest])?;
            } else if sealed_cloud_grant.is_some() {
                return Err(Error::Unauthorized);
            }
            tx.execute("INSERT INTO alt_client_credentials(token_digest,client_id,created_at) VALUES($1,$2,$3)", &[&token_digest(token),&CLI_CLIENT_ID,&integer(time)?])?;
        }
        tx.execute(
            "UPDATE alt_device_authorizations SET state=$2,approved_by=$3 WHERE device_digest=$1",
            &[&device, &next, &credential.digest],
        )?;
        crate::browser::audit_details(
            &mut tx,
            &actor,
            if allow {
                "device.approved"
            } else {
                "device.denied"
            },
            &device,
            &serde_json::json!({"client_id":CLI_CLIENT_ID,"scope":CLI_SCOPE,"credential_id":if allow {Some(token_digest(token))} else {None}}),
        )?;
        let mut result = info(&row)?;
        result.state = next.into();
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
    pub fn poll_device_authorization(&mut self, device_digest: &str) -> Result<DevicePoll> {
        crate::bytes::check_digest(device_digest)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let Some(row) = tx.query_opt(
            "SELECT * FROM alt_device_authorizations WHERE device_digest=$1 FOR UPDATE",
            &[&device_digest],
        )?
        else {
            return Ok(DevicePoll::Waiting(DeviceError::InvalidGrant));
        };
        let time = now(&mut tx)?;
        if unsigned(row.get("expires_at"))? <= time {
            return Ok(DevicePoll::Waiting(DeviceError::ExpiredToken));
        }
        match row.get::<_, String>("state").as_str() {
            "approved" => {
                let digest: String = row.get("credential_digest");
                let row = tx.query_one(
                    "SELECT expires_at,revoked FROM alt_credentials WHERE token_digest=$1",
                    &[&digest],
                )?;
                let expires = unsigned(row.get("expires_at"))?;
                if row.get::<_, bool>("revoked") || expires <= time {
                    return Ok(DevicePoll::Waiting(DeviceError::AccessDenied));
                }
                tx.commit()?;
                Ok(DevicePoll::Ready {
                    expires_at_unix_ms: expires,
                })
            }
            "denied" => Ok(DevicePoll::Waiting(DeviceError::AccessDenied)),
            "pending" => {
                let early = unsigned(row.get("next_poll_at"))? > time;
                let interval = unsigned(row.get("interval_ms"))?
                    .saturating_add(if early { DEVICE_INTERVAL_MS } else { 0 })
                    .min(DEVICE_LIFETIME_MS);
                tx.execute("UPDATE alt_device_authorizations SET next_poll_at=$2,interval_ms=$3 WHERE device_digest=$1", &[&device_digest,&integer(time.saturating_add(interval))?,&integer(interval)?])?;
                tx.commit()?;
                Ok(DevicePoll::Waiting(if early {
                    DeviceError::SlowDown
                } else {
                    DeviceError::AuthorizationPending
                }))
            }
            _ => Err(Error::Integrity("device authorization state")),
        }
    }
    pub fn client_credentials(&mut self, credential: &Credential) -> Result<Vec<ClientCredential>> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = credential_row(&mut tx, &self.spec, credential)?;
        human(&current)?;
        let actor: String = current.get("actor_id");
        let time = integer(now(&mut tx)?)?;
        let rows = tx.query("SELECT c.token_digest,c.expires_at,m.client_id,m.created_at FROM alt_credentials c JOIN alt_client_credentials m USING(token_digest) WHERE c.actor_id=$1 AND NOT c.revoked AND c.expires_at>$2 ORDER BY m.created_at,c.token_digest LIMIT 100", &[&actor,&time])?;
        let result = rows
            .iter()
            .map(|row| {
                Ok(ClientCredential {
                    id: row.get("token_digest"),
                    client_id: row.get("client_id"),
                    created_at_unix_ms: unsigned(row.get("created_at"))?,
                    expires_at_unix_ms: unsigned(row.get("expires_at"))?,
                    current: row.get::<_, String>("token_digest") == credential.digest,
                })
            })
            .collect::<Result<_>>()?;
        tx.commit()?;
        Ok(result)
    }
    pub fn revoke_client_credential(&mut self, credential: &Credential, id: &str) -> Result<()> {
        crate::bytes::check_digest(id)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = credential_row(&mut tx, &self.spec, credential)?;
        human(&current)?;
        let actor: String = current.get("actor_id");
        if tx.execute("UPDATE alt_credentials c SET revoked=true FROM alt_client_credentials m WHERE c.token_digest=m.token_digest AND c.actor_id=$1 AND c.token_digest=$2", &[&actor,&id])? != 1 {return Err(Error::Unauthorized);}
        crate::browser::audit(&mut tx, &actor, "client.revoke", id)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}
