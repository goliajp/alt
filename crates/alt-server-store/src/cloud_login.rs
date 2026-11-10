use crate::database::Transaction;
use crate::store::{instance, integer, now, unsigned};
use crate::{Error, Result, Store};
use alt_remote_protocol::DeploymentMode;
use golia_platform_contract::AccountAuthorization;

pub enum CloudLoginClaim {
    Exchange { credential_expires_at: u64 },
    Complete,
}
pub(crate) fn check_completion(
    tx: &mut Transaction<'_>,
    id: &str,
    ticket: &str,
    digest: &str,
    expires: u64,
) -> Result<()> {
    let row = tx.query_opt("SELECT state,ticket_digest,credential_digest,credential_expires_at,expires_at FROM alt_cloud_logins WHERE id=$1 FOR UPDATE", &[&id])?.ok_or(Error::Unauthorized)?;
    if row.get::<_, String>("state") != "exchanging"
        || row.get::<_, Option<String>>("ticket_digest").as_deref() != Some(ticket)
        || row.get::<_, String>("credential_digest") != digest
        || unsigned(row.get("credential_expires_at"))? != expires
        || unsigned(row.get("expires_at"))? <= now(tx)?
    {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
impl Store {
    pub fn begin_cloud_browser_login(
        &mut self,
        id: &str,
        credential_digest: &str,
        ttl_ms: u64,
        peer: std::net::IpAddr,
    ) -> Result<()> {
        crate::bytes::check_digest(id)?;
        crate::bytes::check_digest(credential_digest)?;
        if !(60_000..=8 * 3_600_000).contains(&ttl_ms) {
            return Err(Error::Precondition("Cloud session lifetime"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        let admission = crate::browser::login_budget(&mut tx, id, peer);
        if admission.is_err() {
            tx.commit()?;
            return admission;
        }
        let time = now(&mut tx)?;
        tx.execute(
            "DELETE FROM alt_cloud_logins WHERE expires_at<=$1",
            &[&integer(time)?],
        )?;
        tx.execute("INSERT INTO alt_cloud_logins(id,credential_digest,credential_expires_at,expires_at,state) VALUES($1,$2,$3,$4,'pending')", &[&id,&credential_digest,&integer(time.saturating_add(ttl_ms))?,&integer(time.saturating_add(300_000))?])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    pub fn claim_cloud_browser_login(
        &mut self,
        id: &str,
        ticket_digest: &str,
    ) -> Result<CloudLoginClaim> {
        crate::bytes::check_digest(id)?;
        crate::bytes::check_digest(ticket_digest)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        let row = tx.query_opt("SELECT state,ticket_digest,credential_expires_at,expires_at FROM alt_cloud_logins WHERE id=$1 FOR UPDATE", &[&id])?.ok_or(Error::Unauthorized)?;
        if unsigned(row.get("expires_at"))? <= now(&mut tx)? {
            return Err(Error::Unauthorized);
        }
        let result = match row.get::<_, String>("state").as_str() {
            "pending" => {
                tx.execute(
                    "UPDATE alt_cloud_logins SET state='exchanging',ticket_digest=$2 WHERE id=$1",
                    &[&id, &ticket_digest],
                )?;
                CloudLoginClaim::Exchange {
                    credential_expires_at: unsigned(row.get("credential_expires_at"))?,
                }
            }
            "complete"
                if row.get::<_, Option<String>>("ticket_digest").as_deref()
                    == Some(ticket_digest) =>
            {
                CloudLoginClaim::Complete
            }
            "exchanging"
                if row.get::<_, Option<String>>("ticket_digest").as_deref()
                    == Some(ticket_digest) =>
            {
                return Err(Error::OutcomeUnknown);
            }
            _ => return Err(Error::Unauthorized),
        };
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
    pub fn refuse_cloud_browser_login(&mut self, id: &str, ticket_digest: &str) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        if self.spec.deployment_mode != DeploymentMode::Cloud {
            return Err(Error::Unauthorized);
        }
        if tx.execute("UPDATE alt_cloud_logins SET state='refused' WHERE id=$1 AND ticket_digest=$2 AND state='exchanging'", &[&id,&ticket_digest])? != 1 { return Err(Error::Unauthorized); }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn complete_cloud_browser_login(
        &mut self,
        id: &str,
        ticket_digest: &str,
        token: &str,
        sealed_grant: &str,
        claims: &AccountAuthorization,
        expires: u64,
    ) -> Result<()> {
        self.accept_cloud_login_inner(
            token,
            sealed_grant,
            claims,
            expires,
            true,
            Some((id, ticket_digest)),
        )
    }
}
