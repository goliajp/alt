//! Product browser sessions retain platform identity without local accounts.
use crate::Jsonb;
use crate::database::Transaction;
use crate::store::{credential_row, instance, integer, now, unsigned};
use crate::{ActiveBrowserSession, BrowserSession, Credential, Error, LocalUser, Result, Store};
use alt_browser_auth::PublicOrigin;
use golia_platform_contract::AccountAuthorization;

fn browser(tx: &mut Transaction<'_>, digest: &str) -> Result<postgres::Row> {
    tx.query_opt("SELECT p.claims,p.created_at,c.actor_id,c.expires_at,c.administrator FROM alt_cloud_credentials p JOIN alt_credentials c USING(token_digest) WHERE p.token_digest=$1 AND p.browser", &[&digest])?
        .ok_or(Error::Unauthorized)
}
impl Store {
    pub(crate) fn cloud_browser_session(
        &mut self,
        credential: &Credential,
        origin: &PublicOrigin,
    ) -> Result<BrowserSession> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        crate::browser::binding(&mut tx, origin)?;
        credential_row(&mut tx, &self.spec, credential)?;
        let row = browser(&mut tx, &credential.digest)?;
        let claims: AccountAuthorization = row.get::<_, Jsonb>("claims").read_serde()?;
        let result = BrowserSession {
            user: LocalUser {
                actor_id: claims.account_id.as_str().into(),
                login_name: claims.account_id.as_str().into(),
                display_name: claims.display_name,
                administrator: row.get("administrator"),
                disabled: false,
                generation: claims.lease.revocation_generation.0,
            },
            expires_at_unix_ms: unsigned(row.get("expires_at"))?,
        };
        tx.commit()?;
        Ok(result)
    }
    pub(crate) fn cloud_browser_sessions(
        &mut self,
        credential: &Credential,
        origin: &PublicOrigin,
    ) -> Result<Vec<ActiveBrowserSession>> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        crate::browser::binding(&mut tx, origin)?;
        credential_row(&mut tx, &self.spec, credential)?;
        let actor: String = browser(&mut tx, &credential.digest)?.get("actor_id");
        let time = integer(now(&mut tx)?)?;
        let rows = tx.query("SELECT c.token_digest,p.created_at,c.expires_at FROM alt_cloud_credentials p JOIN alt_credentials c USING(token_digest) WHERE p.browser AND c.actor_id=$1 AND NOT c.revoked AND c.expires_at>$2 ORDER BY p.created_at,c.token_digest LIMIT 16", &[&actor,&time])?;
        let mut result = Vec::new();
        for row in rows {
            let id: String = row.get(0);
            result.push(ActiveBrowserSession {
                current: id == credential.digest,
                id,
                created_at_unix_ms: unsigned(row.get(1))?.to_string(),
                expires_at_unix_ms: unsigned(row.get(2))?.to_string(),
            });
        }
        tx.commit()?;
        Ok(result)
    }
    pub(crate) fn revoke_cloud_browser_session(
        &mut self,
        credential: &Credential,
        origin: &PublicOrigin,
        id: &str,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        crate::browser::binding(&mut tx, origin)?;
        credential_row(&mut tx, &self.spec, credential)?;
        let actor: String = browser(&mut tx, &credential.digest)?.get("actor_id");
        let target = browser(&mut tx, id)?;
        if target.get::<_, String>("actor_id") != actor {
            return Err(Error::Unauthorized);
        }
        tx.execute(
            "UPDATE alt_credentials SET revoked=true WHERE token_digest=$1",
            &[&id],
        )?;
        crate::browser::audit(&mut tx, &actor, "cloud.session.revoke", id)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}
