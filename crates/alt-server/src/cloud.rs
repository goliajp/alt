//! Server-owned platform transport. Network operations never hold a product
//! database transaction; every resulting lease is checked again by the store.
use alt_cloud_client::{Client, GrantSealer};
use alt_server_store::{Error, Result, Store};
use golia_platform_contract::{Audience, Secret};

pub struct Runtime {
    pub(crate) client: Client,
    pub(crate) sealer: GrantSealer,
    key_digest: String,
    provision_digest: Option<String>,
}
pub(crate) fn clock() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| value.as_millis().try_into().unwrap_or(u64::MAX))
}
impl Runtime {
    pub fn new(audience: Audience, credential: Secret) -> alt_cloud_client::Result<Self> {
        let key_digest = blake3::hash(credential.expose().as_bytes())
            .to_hex()
            .to_string();
        Ok(Self {
            sealer: GrantSealer::new(audience.clone(), &credential)?,
            client: Client::new(audience, credential)?,
            key_digest,
            provision_digest: None,
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
        use subtle::ConstantTimeEq;
        golia_platform_contract::lifecycle::lower_hex_32(credential)
            && self
                .key_digest
                .as_bytes()
                .ct_eq(blake3::hash(credential.as_bytes()).to_hex().as_bytes())
                .into()
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
        if store.cloud_service_refresh_needed()? {
            match self.client.service(clock) {
                Ok(service) => store.refresh_cloud_service(&service)?,
                Err(alt_cloud_client::Error::Unauthorized) => store.revoke_cloud_service()?,
                Err(_) => (), // Never extend the persisted lease on failure.
            }
        }
        // Service refresh may have advanced the shared revocation generation.
        let descriptor = store.cloud_refresh(token)?;
        if descriptor.refresh_needed {
            let grant = self
                .sealer
                .open(&descriptor.sealed_grant, &descriptor.credential_digest)
                .map_err(|_| Error::Unauthorized)?;
            match self.client.introspect(&grant, clock) {
                Ok(claims) => {
                    store.refresh_cloud_account(&descriptor.credential_digest, &claims)?
                }
                Err(alt_cloud_client::Error::Unauthorized) => {
                    store.revoke_cloud_credential(&descriptor.credential_digest)?;
                    return Err(Error::Unauthorized);
                }
                Err(_) => match store.authenticate(token) {
                    Ok(_) => (),
                    Err(Error::Unauthorized) => return Err(Error::AuthorityUnavailable),
                    Err(error) => return Err(error),
                },
            }
        }
        Ok(())
    }
}
