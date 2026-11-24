//! The credentials delegated to agents: issued, read, listed and revoked.

use super::*;

impl Transport {
    pub fn issue_agent(
        &self,
        request: &delegation::DelegationRequest,
    ) -> Result<delegation::DelegationInfo> {
        request.validate()?;
        self.json("POST", "/v1/delegations", &self.encode(request)?, None)
    }
    pub fn agent_delegation(&self, credential_id: &str) -> Result<delegation::DelegationInfo> {
        digest(credential_id)?;
        self.json(
            "GET",
            &format!("/v1/delegations/{credential_id}"),
            &[],
            None,
        )
    }
    pub fn agent_delegations(&self, after: Option<&str>) -> Result<delegation::DelegationPage> {
        let path = if let Some(after) = after {
            digest(after)?;
            format!("/v1/delegations?after={after}")
        } else {
            "/v1/delegations".into()
        };
        self.json("GET", &path, &[], None)
    }
    pub fn revoke_agent(&self, credential_id: &str) -> Result<()> {
        digest(credential_id)?;
        let value: alt_json::Value = self.json(
            "POST",
            &format!("/v1/delegations/{credential_id}/revoke"),
            &[],
            None,
        )?;
        if value.get("revoked").and_then(|v| v.as_bool()) != Some(true)
            || value.get("credential_id").and_then(|v| v.as_str()) != Some(credential_id)
        {
            return Err(Error::Invalid("delegation revocation response"));
        }
        Ok(())
    }
}
