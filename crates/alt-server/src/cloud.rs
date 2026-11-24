//! Server-owned platform transport. Network operations never hold a product
//! database transaction; every resulting lease is checked again by the store.
use alt_cloud_client::{Client, GrantSealer};
use alt_server_store::{Error, Renewal, Result, Store};
use asking::Asked;
use golia_platform_contract::{Audience, Secret};

mod asking;

pub struct Runtime {
    pub(crate) client: Client,
    pub(crate) sealer: GrantSealer,
    key_digest: String,
    provision_digest: Option<String>,
    asking: asking::Asking,
}
pub(crate) fn clock() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| value.as_millis().try_into().unwrap_or(u64::MAX))
}
impl Runtime {
    pub fn new(audience: Audience, credential: Secret) -> alt_cloud_client::Result<Self> {
        let key_digest = alt_hash::blake3::hash(credential.expose().as_bytes())
            .to_hex()
            .to_string();
        Ok(Self {
            sealer: GrantSealer::new(audience.clone(), &credential)?,
            client: Client::new(audience, credential)?,
            key_digest,
            provision_digest: None,
            asking: asking::Asking::default(),
        })
    }
    pub fn provisioned(mut self, digest: String) -> alt_cloud_client::Result<Self> {
        if !golia_platform_contract::lifecycle::lower_hex_32(&digest) {
            return Err(alt_cloud_client::Error::Unauthorized);
        }
        self.provision_digest = Some(digest);
        Ok(self)
    }
    pub fn provision_digest(&self) -> Option<&str> {
        self.provision_digest.as_deref()
    }
    pub fn authenticate_probe(&self, credential: &str) -> bool {
        golia_platform_contract::lifecycle::lower_hex_32(credential)
            && alt_crypto::ct::eq(
                self.key_digest.as_bytes(),
                alt_hash::blake3::hash(credential.as_bytes())
                    .to_hex()
                    .as_bytes(),
            )
    }
    pub fn readiness(
        &self,
        store: &mut Store,
        probe: golia_platform_contract::lifecycle::Probe,
    ) -> Result<golia_platform_contract::lifecycle::Readiness> {
        probe.validate().map_err(|_| Error::Unauthorized)?;
        if &probe.audience != self.audience()
            || self.provision_digest() != Some(probe.spec_digest.as_str())
        {
            return Err(Error::Unauthorized);
        }
        self.check(store)?;
        store.cloud_readiness(&probe.spec_digest)?;
        if let Some(digest) = &probe.recovery_manifest_digest {
            store.check_restored_backup(digest)?;
        }
        Ok(golia_platform_contract::lifecycle::Readiness {
            probe,
            database: true,
            blob_storage: true,
            durable_storage: true,
        })
    }
    pub fn audience(&self) -> &Audience {
        self.client.audience()
    }
    pub fn key_digest(&self) -> &str {
        &self.key_digest
    }
    pub fn check(&self, store: &mut Store) -> Result<()> {
        store.check_cloud_binding(self.audience(), &self.key_digest)?;
        store.check_cloud_provision(self.provision_digest())
    }
    pub fn navigation(
        &self,
        store: &mut Store,
        token: &str,
    ) -> Result<golia_platform_contract::navigation::PublicNavigation> {
        self.check(store)?;
        let descriptor = store.cloud_refresh(token)?;
        if !descriptor.browser {
            return Err(Error::Unauthorized);
        }
        let grant = self
            .sealer
            .open(&descriptor.sealed_grant, &descriptor.credential_digest)
            .map_err(|_| Error::Unauthorized)?;
        let value = match self
            .client
            .navigation(&grant, &descriptor.claims.account_id, clock)
        {
            Ok(value) => value,
            Err(alt_cloud_client::Error::Unauthorized) => {
                store.revoke_cloud_credential(&descriptor.credential_digest)?;
                return Err(Error::Unauthorized);
            }
            Err(_) => return Err(Error::AuthorityUnavailable),
        };
        store.refresh_cloud_account(&descriptor.credential_digest, &value.account)?;
        store.authenticate(token)?;
        Ok(value.public_view())
    }
    pub fn refresh(&self, store: &mut Store, token: &str) -> Result<()> {
        // Check that a local credential exists before admitting control traffic.
        store.cloud_refresh(token)?;
        loop {
            let renewal = store.cloud_service_renewal()?;
            if renewal == Renewal::Fresh {
                break;
            }
            match self.asking.turn(None, renewal == Renewal::Due) {
                Asked::Turn(turn) => match self.client.service(clock) {
                    Ok(service) => {
                        turn.answered();
                        store.refresh_cloud_service(&service)?
                    }
                    Err(alt_cloud_client::Error::Unauthorized) => {
                        turn.answered();
                        store.revoke_cloud_service()?
                    }
                    Err(_) => (), // Never extend the persisted lease on failure.
                },
                Asked::Look => continue,
                Asked::No => (),
            }
            break;
        }
        // Service refresh may have advanced the shared revocation generation.
        loop {
            let descriptor = store.cloud_refresh(token)?;
            if !descriptor.refresh_needed {
                break;
            }
            let digest = &descriptor.credential_digest;
            // whoever has no turn to ask goes on as one whose asking failed
            let asked = match self.asking.turn(Some(digest), !descriptor.serves) {
                Asked::Look => continue,
                Asked::No => None,
                Asked::Turn(turn) => {
                    let grant = self.sealer.open(&descriptor.sealed_grant, digest);
                    let grant = grant.map_err(|_| Error::Unauthorized)?;
                    let told = self.client.introspect(&grant, clock);
                    if matches!(told, Ok(_) | Err(alt_cloud_client::Error::Unauthorized)) {
                        turn.answered();
                    }
                    Some(told)
                }
            };
            match asked {
                Some(Ok(claims)) => store.refresh_cloud_account(digest, &claims)?,
                Some(Err(alt_cloud_client::Error::Unauthorized)) => {
                    store.revoke_cloud_credential(digest)?;
                    return Err(Error::Unauthorized);
                }
                Some(Err(_)) | None => match store.authenticate(token) {
                    Ok(_) => (),
                    Err(Error::Unauthorized) => return Err(Error::AuthorityUnavailable),
                    Err(error) => return Err(error),
                },
            }
            break;
        }
        Ok(())
    }
}
