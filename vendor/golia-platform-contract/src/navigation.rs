//! A navigation snapshot is current account metadata, never a content credential.
use crate::{AccountAuthorization, Audience, Error, Id, Product, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub audience: Audience,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Navigation {
    pub account: AccountAuthorization,
    pub workspace_name: String,
    pub entries: Vec<Entry>,
}
fn label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 320 && !value.chars().any(char::is_control)
}
impl Navigation {
    pub fn check(&self, expected: &Audience, account: &Id, now_unix_ms: u64) -> Result<()> {
        self.account.check(expected, now_unix_ms)?;
        if &self.account.account_id != account
            || !label(&self.workspace_name)
            || self.entries.is_empty()
            || self.entries.len() > 2
        {
            return Err(Error::Invalid("navigation subject or workspace"));
        }
        let mut products = Vec::<Product>::new();
        let mut installations = Vec::new();
        let mut origins = Vec::new();
        for entry in &self.entries {
            entry.audience.validate()?;
            if !label(&entry.name)
                || entry.audience.organization_id != expected.organization_id
                || entry.audience.workspace_id != expected.workspace_id
                || entry.audience.control_origin != expected.control_origin
                || products.contains(&entry.audience.product)
                || installations.contains(&entry.audience.installation_id)
                || origins.contains(&entry.audience.origin)
            {
                return Err(Error::Invalid("navigation entry scope"));
            }
            products.push(entry.audience.product);
            installations.push(entry.audience.installation_id.clone());
            origins.push(entry.audience.origin.clone());
        }
        if !self.entries.iter().any(|entry| &entry.audience == expected) {
            return Err(Error::Invalid("navigation current installation"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicNavigation {
    pub workspace_id: Id,
    pub workspace_name: String,
    pub control_url: String,
    pub products: Vec<PublicEntry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicEntry {
    pub product: Product,
    pub installation_id: Id,
    pub name: String,
    pub url: String,
}
impl Navigation {
    /// Call only after `check`; browser output omits all upstream grant and
    /// account lease material. Entry destinations always initiate fresh login.
    pub fn public_view(&self) -> PublicNavigation {
        let a = &self.account.lease.audience;
        PublicNavigation {
            workspace_id: a.workspace_id.clone(),
            workspace_name: self.workspace_name.clone(),
            control_url: format!(
                "{}/cloud?organization={}",
                a.control_origin.as_str(),
                a.organization_id.as_str()
            ),
            products: self
                .entries
                .iter()
                .map(|entry| PublicEntry {
                    product: entry.audience.product,
                    installation_id: entry.audience.installation_id.clone(),
                    name: entry.name.clone(),
                    url: format!(
                        "{}{}",
                        entry.audience.origin.as_str(),
                        match entry.audience.product {
                            Product::Alt => "/web/login/start",
                            Product::Sentori => "/auth/cloud/start",
                        }
                    ),
                })
                .collect(),
        }
    }
}
