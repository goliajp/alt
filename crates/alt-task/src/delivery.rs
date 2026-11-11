use crate::{Artifact, Error, Record, Result, check_id};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(
    Clone, Debug, PartialEq, Eq, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson,
)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct TaskBaseline {
    pub schema_version: u32,
    pub task: String,
    pub snapshot: Artifact,
    pub creator_request: String,
}
impl Record for TaskBaseline {
    const KIND: &'static str = "task-baseline";
    fn validate(&self) -> Result<()> {
        header(self.schema_version, &self.creator_request)?;
        check_id(&self.task)?;
        check_id(&self.snapshot.blake3)?;
        Ok(())
    }
}
#[derive(
    Clone, Debug, PartialEq, Eq, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson,
)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct Review {
    pub schema_version: u32,
    pub task: String,
    pub candidate: String,
    pub evidence: String,
    pub baseline: String,
    pub target_expected: BTreeMap<String, String>,
    pub reviewer: String,
    pub reason: String,
    pub commit_message: String,
    /// Complete, byte-preserving before/after changes, including binary files.
    pub changes: Artifact,
    pub creator_request: String,
}
impl Record for Review {
    const KIND: &'static str = "review";
    fn validate(&self) -> Result<()> {
        header(self.schema_version, &self.creator_request)?;
        for id in [
            &self.task,
            &self.candidate,
            &self.evidence,
            &self.baseline,
            &self.changes.blake3,
        ] {
            check_id(id)?;
        }
        if self.reviewer.trim().is_empty()
            || self.reason.trim().is_empty()
            || self.commit_message.trim().is_empty()
            || self.commit_message.contains('\0')
            || self.target_expected.len() != 1
        {
            return Err(Error::Invalid(
                "review requires attribution, rationale, message and one target".into(),
            ));
        }
        Ok(())
    }
}
#[derive(
    Clone, Debug, PartialEq, Eq, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson,
)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct Integration {
    pub schema_version: u32,
    pub task: String,
    pub review: String,
    pub commit: String,
    pub target: String,
    pub before: String,
    pub notes_before: Option<String>,
    pub notes_after: String,
    pub creator_request: String,
}
impl Record for Integration {
    const KIND: &'static str = "integration";
    fn validate(&self) -> Result<()> {
        header(self.schema_version, &self.creator_request)?;
        check_id(&self.task)?;
        check_id(&self.review)?;
        if !self.target.starts_with("refs/heads/")
            || self.before.is_empty()
            || self.commit.is_empty()
        {
            return Err(Error::Invalid("incomplete integration intent".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct Delivery {
    pub schema_version: u32,
    pub task: String,
    pub integration: String,
    pub destination: String,
    pub report: Artifact,
    pub creator_request: String,
}
impl Record for Delivery {
    const KIND: &'static str = "delivery";
    fn validate(&self) -> Result<()> {
        header(self.schema_version, &self.creator_request)?;
        for id in [&self.task, &self.integration, &self.report.blake3] {
            check_id(id)?;
        }
        if self.destination.is_empty() {
            return Err(Error::Invalid("delivery destination missing".into()));
        }
        Ok(())
    }
}
fn header(version: u32, request: &str) -> Result<()> {
    if version != 1 || request.is_empty() {
        return Err(Error::Invalid(
            "unsupported or incomplete delivery record".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct DeliveryAttempt {
    pub schema_version: u32,
    pub task: String,
    pub integration: String,
    pub destination: String,
    pub creator_request: String,
}
impl Record for DeliveryAttempt {
    const KIND: &'static str = "delivery-attempt";
    fn validate(&self) -> Result<()> {
        header(self.schema_version, &self.creator_request)?;
        check_id(&self.task)?;
        check_id(&self.integration)?;
        if self.destination.is_empty() {
            return Err(Error::Invalid("delivery destination missing".into()));
        }
        Ok(())
    }
}

#[derive(
    Clone, Debug, PartialEq, Eq, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson,
)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct IntegrationReceipt {
    pub schema_version: u32,
    pub integration: String,
    pub ref_operation: String,
    pub creator_request: String,
}
impl Record for IntegrationReceipt {
    const KIND: &'static str = "integration-receipt";
    fn validate(&self) -> Result<()> {
        header(self.schema_version, &self.creator_request)?;
        check_id(&self.integration)?;
        check_id(&self.ref_operation)?;
        Ok(())
    }
}

#[derive(
    Clone, Debug, PartialEq, Eq, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson,
)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct DeliveryReceipt {
    pub schema_version: u32,
    pub delivery: String,
    pub verification: Artifact,
    pub creator_request: String,
}
impl Record for DeliveryReceipt {
    const KIND: &'static str = "delivery-receipt";
    fn validate(&self) -> Result<()> {
        header(self.schema_version, &self.creator_request)?;
        check_id(&self.delivery)?;
        check_id(&self.verification.blake3)?;
        Ok(())
    }
}
