use crate::{Error, Result, check_id};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub trait Record: Serialize + serde::de::DeserializeOwned {
    const KIND: &'static str;
    fn validate(&self) -> Result<()>;
    fn id(&self) -> Result<String> {
        self.validate()?;
        Ok(alt_input::identity(Self::KIND, self)?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub repository: String,
    pub workspace: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub head: String,
    pub tree: String,
    pub refs: BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub purpose: String,
    pub authority: String,
    pub allowed_paths: Vec<String>,
    pub auxiliary: Vec<alt_input::InputRoot>,
    pub exclusions: Vec<String>,
    pub validation: alt_input::Context,
    /// Original signed request and acceptance. This preserves historical
    /// attribution only; the caller separately checks current remote authority
    /// and claims the exact planned Task before creating a source-linked task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_source: Option<Box<golia_change_link::RepairSource>>,
}
impl TaskSpec {
    pub fn validate(&self) -> Result<()> {
        if let Some(source) = &self.repair_source {
            source
                .validate()
                .map_err(|_| Error::Invalid("invalid repair source proof".into()))?;
        }
        if self.purpose.trim().is_empty()
            || self.authority.trim().is_empty()
            || self.allowed_paths.is_empty()
        {
            return Err(Error::Invalid(
                "purpose, authority and allowed paths are required".into(),
            ));
        }
        for path in self
            .allowed_paths
            .iter()
            .chain(&self.exclusions)
            .chain(self.auxiliary.iter().map(|root| &root.path))
        {
            if path != "." {
                alt_input::checked_path(path.as_bytes())?;
            }
        }
        Ok(())
    }
    pub fn allows(&self, path: &str) -> bool {
        self.allowed_paths.iter().any(|prefix| {
            prefix == "."
                || path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub schema_version: u32,
    pub scope: Scope,
    pub spec: TaskSpec,
    pub baseline: Baseline,
    pub target_expected: BTreeMap<String, String>,
    pub creator_request: String,
}
impl Record for Task {
    const KIND: &'static str = "task";
    fn validate(&self) -> Result<()> {
        version(self.schema_version)?;
        self.spec.validate()?;
        if let Some(source) = &self.spec.repair_source {
            let golia_change_link::Body::RepairRequest(request) = &source.request.event.body else {
                return Err(Error::Invalid(
                    "repair source must be a repair request".into(),
                ));
            };
            if self.target_expected.len() != 1
                || !self.target_expected.contains_key(&request.target_ref)
            {
                return Err(Error::Invalid(
                    "repair target differs from the signed request".into(),
                ));
            }
        }
        if self.scope.repository.is_empty()
            || self.scope.workspace.is_empty()
            || self.creator_request.is_empty()
            || self.baseline.head.is_empty()
            || self.baseline.tree.is_empty()
            || self.target_expected.is_empty()
        {
            return Err(Error::Invalid(
                "incomplete task scope/baseline/target".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub schema_version: u32,
    pub task: String,
    pub parent: Option<String>,
    pub tree: String,
    pub source_selection: String,
    pub manifest: String,
    pub target_expected: BTreeMap<String, String>,
    pub creator_request: String,
}
impl Record for Candidate {
    const KIND: &'static str = "candidate";
    fn validate(&self) -> Result<()> {
        version(self.schema_version)?;
        check_id(&self.task)?;
        check_id(&self.manifest)?;
        if let Some(parent) = &self.parent {
            check_id(parent)?;
        }
        if self.source_selection != "index"
            || self.tree.is_empty()
            || self.creator_request.is_empty()
        {
            return Err(Error::Invalid("unsupported or incomplete candidate".into()));
        }
        Ok(())
    }
}
impl Record for alt_input::Manifest {
    const KIND: &'static str = "manifest";
    fn validate(&self) -> Result<()> {
        Ok(self.validate()?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub blake3: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Termination {
    Exit { code: i32 },
    Signal { number: i32 },
    Timeout,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Reported,
    Observed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub schema_version: u32,
    pub candidate: String,
    pub manifest: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub runner: String,
    pub actual_context: alt_input::Context,
    pub binaries: BTreeMap<String, Artifact>,
    pub started_unix_ns: u64,
    pub finished_unix_ns: u64,
    pub termination: Termination,
    pub stdout: Option<Artifact>,
    pub stderr: Option<Artifact>,
    pub artifacts: BTreeMap<String, Artifact>,
    pub provenance: Provenance,
    /// Preserve the external report as received, even when inconsistent.
    pub original_report: Option<Artifact>,
    pub creator_request: String,
}
impl Record for Evidence {
    const KIND: &'static str = "evidence";
    fn validate(&self) -> Result<()> {
        version(self.schema_version)?;
        check_id(&self.candidate)?;
        check_id(&self.manifest)?;
        for id in [&self.before, &self.after].into_iter().flatten() {
            check_id(id)?;
        }
        for artifact in self
            .binaries
            .values()
            .chain(self.artifacts.values())
            .chain(self.stdout.iter())
            .chain(self.stderr.iter())
            .chain(self.original_report.iter())
        {
            check_id(&artifact.blake3)?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub schema_version: u32,
    pub task: String,
    pub previous: Option<String>,
    pub state: TaskState,
    pub reason: String,
    pub creator_request: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Abandoned,
    Integrated,
}
impl Record for Event {
    const KIND: &'static str = "event";
    fn validate(&self) -> Result<()> {
        version(self.schema_version)?;
        check_id(&self.task)?;
        if let Some(previous) = &self.previous {
            check_id(previous)?;
        }
        if self.reason.trim().is_empty() || self.creator_request.is_empty() {
            return Err(Error::Invalid("event reason/request required".into()));
        }
        Ok(())
    }
}
fn version(version: u32) -> Result<()> {
    if version != 1 {
        return Err(Error::Invalid("unsupported record version".into()));
    }
    Ok(())
}
