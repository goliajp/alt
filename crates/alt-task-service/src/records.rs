use crate::{Error, Result, strict_json};
use alt_task::Record;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, alt_json::ToJson, alt_json::FromJson)]
#[json(rename_all = "kebab-case")]
pub enum RecordKind {
    Task,
    Candidate,
    Manifest,
    Evidence,
    Review,
    Integration,
    Delivery,
    TaskBaseline,
    Event,
    IntegrationReceipt,
    DeliveryReceipt,
}
impl RecordKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Candidate => "candidate",
            Self::Manifest => "manifest",
            Self::Evidence => "evidence",
            Self::Review => "review",
            Self::Integration => "integration",
            Self::Delivery => "delivery",
            Self::TaskBaseline => "task-baseline",
            Self::Event => "event",
            Self::IntegrationReceipt => "integration-receipt",
            Self::DeliveryReceipt => "delivery-receipt",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct RecordKey {
    pub kind: RecordKind,
    pub id: String,
}
impl RecordKey {
    pub fn validate(&self) -> Result<()> {
        alt_task::check_id(&self.id)?;
        Ok(())
    }
}

/// The existing source envelope, kept byte-for-byte in storage. The wire entry
/// names both its raw payload digest and this record's domain-separated ID.
#[derive(alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct Envelope {
    schema_version: u32,
    hash_algorithm: String,
    kind: RecordKind,
    record: alt_json::Value,
}
#[derive(Clone, Debug)]
pub enum TaskRecord {
    Task(alt_task::Task),
    Candidate(alt_task::Candidate),
    Manifest(alt_input::Manifest),
    Evidence(alt_task::Evidence),
    Review(alt_task::Review),
    Integration(alt_task::Integration),
    Delivery(alt_task::Delivery),
    TaskBaseline(alt_task::TaskBaseline),
    Event(alt_task::Event),
    IntegrationReceipt(alt_task::IntegrationReceipt),
    DeliveryReceipt(alt_task::DeliveryReceipt),
}
macro_rules! dispatch {
    ($self:expr, $v:ident => $body:expr) => {
        match $self {
            TaskRecord::Task($v) => $body,
            TaskRecord::Candidate($v) => $body,
            TaskRecord::Manifest($v) => $body,
            TaskRecord::Evidence($v) => $body,
            TaskRecord::Review($v) => $body,
            TaskRecord::Integration($v) => $body,
            TaskRecord::Delivery($v) => $body,
            TaskRecord::TaskBaseline($v) => $body,
            TaskRecord::Event($v) => $body,
            TaskRecord::IntegrationReceipt($v) => $body,
            TaskRecord::DeliveryReceipt($v) => $body,
        }
    };
}
impl TaskRecord {
    pub fn kind(&self) -> RecordKind {
        match self {
            Self::Task(_) => RecordKind::Task,
            Self::Candidate(_) => RecordKind::Candidate,
            Self::Manifest(_) => RecordKind::Manifest,
            Self::Evidence(_) => RecordKind::Evidence,
            Self::Review(_) => RecordKind::Review,
            Self::Integration(_) => RecordKind::Integration,
            Self::Delivery(_) => RecordKind::Delivery,
            Self::TaskBaseline(_) => RecordKind::TaskBaseline,
            Self::Event(_) => RecordKind::Event,
            Self::IntegrationReceipt(_) => RecordKind::IntegrationReceipt,
            Self::DeliveryReceipt(_) => RecordKind::DeliveryReceipt,
        }
    }
    pub fn decode(key: &RecordKey, bytes: &[u8], limit: usize) -> Result<Self> {
        key.validate()?;
        let envelope: Envelope = alt_json::from_value(strict_json::decode(bytes, limit)?)?;
        if envelope.schema_version != 1
            || envelope.hash_algorithm != "blake3"
            || envelope.kind != key.kind
        {
            return Err(Error::Integrity("record envelope identity"));
        }
        let v = envelope.record;
        let record = match key.kind {
            RecordKind::Task => Self::Task(alt_json::from_value(v)?),
            RecordKind::Candidate => Self::Candidate(alt_json::from_value(v)?),
            RecordKind::Manifest => Self::Manifest(alt_json::from_value(v)?),
            RecordKind::Evidence => Self::Evidence(alt_json::from_value(v)?),
            RecordKind::Review => Self::Review(alt_json::from_value(v)?),
            RecordKind::Integration => Self::Integration(alt_json::from_value(v)?),
            RecordKind::Delivery => Self::Delivery(alt_json::from_value(v)?),
            RecordKind::TaskBaseline => Self::TaskBaseline(alt_json::from_value(v)?),
            RecordKind::Event => Self::Event(alt_json::from_value(v)?),
            RecordKind::IntegrationReceipt => Self::IntegrationReceipt(alt_json::from_value(v)?),
            RecordKind::DeliveryReceipt => Self::DeliveryReceipt(alt_json::from_value(v)?),
        };
        if record.id()? != key.id {
            return Err(Error::Integrity("canonical record identity"));
        }
        Ok(record)
    }
    pub fn id(&self) -> Result<String> {
        Ok(dispatch!(self,v=>Record::id(v))?)
    }
    pub fn creator_request(&self) -> Option<&str> {
        Some(match self {
            Self::Task(v) => &v.creator_request,
            Self::Candidate(v) => &v.creator_request,
            Self::Evidence(v) => &v.creator_request,
            Self::Review(v) => &v.creator_request,
            Self::Integration(v) => &v.creator_request,
            Self::Delivery(v) => &v.creator_request,
            Self::TaskBaseline(v) => &v.creator_request,
            Self::Event(v) => &v.creator_request,
            Self::IntegrationReceipt(v) => &v.creator_request,
            Self::DeliveryReceipt(v) => &v.creator_request,
            Self::Manifest(_) => return None,
        })
    }
    pub fn dependencies(&self) -> Vec<RecordKey> {
        let mut edges = Vec::new();
        let mut add = |kind, id: &String| {
            edges.push(RecordKey {
                kind,
                id: id.clone(),
            })
        };
        match self {
            Self::Task(_) | Self::Manifest(_) => {}
            Self::Candidate(v) => {
                add(RecordKind::Task, &v.task);
                add(RecordKind::Manifest, &v.manifest);
                if let Some(parent) = &v.parent {
                    add(RecordKind::Candidate, parent);
                }
            }
            Self::Evidence(v) => {
                add(RecordKind::Candidate, &v.candidate);
                add(RecordKind::Manifest, &v.manifest);
                for id in [&v.before, &v.after].into_iter().flatten() {
                    add(RecordKind::Manifest, id);
                }
            }
            Self::Review(v) => {
                add(RecordKind::Task, &v.task);
                add(RecordKind::Candidate, &v.candidate);
                add(RecordKind::Evidence, &v.evidence);
                add(RecordKind::TaskBaseline, &v.baseline);
            }
            Self::Integration(v) => {
                add(RecordKind::Task, &v.task);
                add(RecordKind::Review, &v.review);
            }
            Self::Delivery(v) => {
                add(RecordKind::Task, &v.task);
                add(RecordKind::Integration, &v.integration);
            }
            Self::TaskBaseline(v) => add(RecordKind::Task, &v.task),
            Self::Event(v) => {
                add(RecordKind::Task, &v.task);
                if let Some(id) = &v.previous {
                    add(RecordKind::Event, id);
                }
            }
            Self::IntegrationReceipt(v) => add(RecordKind::Integration, &v.integration),
            Self::DeliveryReceipt(v) => add(RecordKind::Delivery, &v.delivery),
        }
        edges.sort();
        edges.dedup();
        edges
    }
    pub fn artifacts(&self) -> Vec<alt_task::Artifact> {
        let mut artifacts = Vec::new();
        match self {
            Self::Evidence(v) => artifacts.extend(
                v.binaries
                    .values()
                    .chain(v.artifacts.values())
                    .chain(v.stdout.iter())
                    .chain(v.stderr.iter())
                    .chain(v.original_report.iter())
                    .cloned(),
            ),
            Self::Review(v) => artifacts.push(v.changes.clone()),
            Self::Delivery(v) => artifacts.push(v.report.clone()),
            Self::TaskBaseline(v) => artifacts.push(v.snapshot.clone()),
            Self::DeliveryReceipt(v) => artifacts.push(v.verification.clone()),
            Self::Manifest(v) => artifacts.extend(
                v.entries
                    .iter()
                    .filter(|entry| {
                        entry.role == alt_input::InputRole::Auxiliary
                            && entry.node.kind != alt_input::EntryKind::Directory
                    })
                    .map(|entry| alt_task::Artifact {
                        blake3: entry.node.content.clone(),
                        bytes: entry.node.len,
                    }),
            ),
            _ => {}
        }
        artifacts
    }
}
