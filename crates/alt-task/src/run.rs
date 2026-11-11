use crate::{Artifact, Error, Record, Result, check_id};
use serde::{Deserialize, Serialize};

/// Durable launch attribution, written before starting any validation command.
/// A missing whole-command receipt does not authorize a second launch.
#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct RunAttempt {
    pub schema_version: u32,
    pub task: String,
    pub candidate: String,
    pub manifest: String,
    pub nonce: String,
    pub local_directory: String,
    pub remote: Option<RemoteRun>,
    pub observer_path: String,
    pub observer: Artifact,
    pub spec: Artifact,
    pub authentication_key: Artifact,
    pub creator_request: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct RemoteRun {
    pub destination: String,
    pub directory: String,
    pub max_wait_seconds: u64,
    pub sync_program: Artifact,
    pub sync_stdout: Artifact,
    pub sync_stderr: Artifact,
}
impl Record for RunAttempt {
    const KIND: &'static str = "run-attempt";
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.nonce.len() != 32
            || !self.nonce.bytes().all(|b| b.is_ascii_hexdigit())
            || self.creator_request.is_empty()
            || self.local_directory.is_empty()
            || self.observer_path.is_empty()
        {
            return Err(Error::Invalid("invalid run attempt".into()));
        }
        for id in [
            &self.task,
            &self.candidate,
            &self.manifest,
            &self.observer.blake3,
            &self.spec.blake3,
            &self.authentication_key.blake3,
        ] {
            check_id(id)?;
        }
        if let Some(remote) = &self.remote {
            if remote.destination.is_empty()
                || remote.directory.is_empty()
                || remote.max_wait_seconds == 0
            {
                return Err(Error::Invalid("invalid remote attempt".into()));
            }
            for artifact in [
                &remote.sync_program,
                &remote.sync_stdout,
                &remote.sync_stderr,
            ] {
                check_id(&artifact.blake3)?;
            }
        }
        Ok(())
    }
}

/// Collection is separate from launch, so a controller can recover a lost
/// response by reading an existing report without executing the task again.
#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct RunReceipt {
    pub schema_version: u32,
    pub attempt: String,
    pub evidence: String,
    pub creator_request: String,
}
impl Record for RunReceipt {
    const KIND: &'static str = "run-receipt";
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.creator_request.is_empty() {
            return Err(Error::Invalid("invalid run receipt".into()));
        }
        check_id(&self.attempt)?;
        check_id(&self.evidence)?;
        Ok(())
    }
}
