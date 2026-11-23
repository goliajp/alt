//! The credentials a person's devices were given, as that person sees and revokes
//! them.
use crate::device_login::human;
use crate::store::{credential_row, instance, integer, now, unsigned};
use crate::{Credential, Error, Result, Store};

#[derive(alt_json::ToJson)]
pub struct ClientCredential {
    pub id: String,
    pub client_id: String,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub current: bool,
}
impl Store {
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
        if tx.execute("UPDATE alt_credentials c SET revoked=true,revoked_at=COALESCE(revoked_at,floor(extract(epoch FROM clock_timestamp())*1000)::bigint) FROM alt_client_credentials m WHERE c.token_digest=m.token_digest AND c.actor_id=$1 AND c.token_digest=$2", &[&actor,&id])? != 1 {return Err(Error::Unauthorized);}
        crate::browser::audit(&mut tx, &actor, "client.revoke", id)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}
