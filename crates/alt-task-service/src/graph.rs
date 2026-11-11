use crate::{
    ArtifactSource, Error, RecordKey, RecordKind, Result, TaskBundle, TaskLimits, TaskRecord,
};
use alt_git_codec::ObjectId;
use std::collections::{BTreeMap, BTreeSet};

/// Decoded selection before object validation. Construction verifies raw and
/// canonical identities, exact dependency closure and explicit companions.
pub struct PreparedTaskGraph {
    pub(crate) bundle: TaskBundle,
    pub(crate) records: BTreeMap<RecordKey, TaskRecord>,
    pub(crate) limits: TaskLimits,
}
fn root_key(root: &alt_remote_protocol::RecordRoot) -> RecordKey {
    use alt_remote_protocol::RecordKind as K;
    RecordKey {
        id: root.id.clone(),
        kind: match root.kind {
            K::Task => RecordKind::Task,
            K::Candidate => RecordKind::Candidate,
            K::Evidence => RecordKind::Evidence,
            K::Review => RecordKind::Review,
            K::Integration => RecordKind::Integration,
            K::Delivery => RecordKind::Delivery,
        },
    }
}
impl PreparedTaskGraph {
    pub fn load(
        bundle: TaskBundle,
        expected_roots: &[alt_remote_protocol::RecordRoot],
        source: &impl ArtifactSource,
        limits: TaskLimits,
    ) -> Result<Self> {
        bundle.validate(limits)?;
        let actual: BTreeSet<_> = bundle.roots.iter().collect();
        let expected: BTreeSet<_> = expected_roots.iter().collect();
        if actual != expected || expected.len() != expected_roots.len() {
            return Err(Error::Integrity("bundle publication roots"));
        }
        let mut records = BTreeMap::new();
        for selection in &bundle.records {
            let bytes =
                crate::source::read_artifact(source, &selection.payload, limits.structured_bytes)?;
            let record = TaskRecord::decode(&selection.record, &bytes, limits.structured_bytes)?;
            records.insert(selection.record.clone(), record);
        }
        let primary = Self::closure(
            &records,
            bundle.roots.iter().map(root_key).collect(),
            limits,
        )?;
        // A companion may add history/proofs for an already-selected entity;
        // it cannot smuggle an unrelated task via a backward edge.
        for key in &bundle.companions {
            let parent = match records
                .get(key)
                .ok_or(Error::Integrity("missing companion"))?
            {
                TaskRecord::TaskBaseline(v) => RecordKey {
                    kind: RecordKind::Task,
                    id: v.task.clone(),
                },
                TaskRecord::Event(v) => RecordKey {
                    kind: RecordKind::Task,
                    id: v.task.clone(),
                },
                TaskRecord::IntegrationReceipt(v) => RecordKey {
                    kind: RecordKind::Integration,
                    id: v.integration.clone(),
                },
                TaskRecord::DeliveryReceipt(v) => RecordKey {
                    kind: RecordKind::Delivery,
                    id: v.delivery.clone(),
                },
                _ => return Err(Error::Integrity("unsupported companion")),
            };
            if !primary.contains(&parent) {
                return Err(Error::Integrity("unselected companion parent"));
            }
        }
        let mut roots: Vec<_> = bundle.roots.iter().map(root_key).collect();
        roots.extend(bundle.companions.clone());
        let selected = Self::closure(&records, roots, limits)?;
        if selected.len() != records.len() {
            return Err(Error::Integrity("unselected extra task records"));
        }
        let graph = Self {
            bundle,
            records,
            limits,
        };
        graph.verify_record_links()?;
        Ok(graph)
    }
    fn closure(
        records: &BTreeMap<RecordKey, TaskRecord>,
        roots: Vec<RecordKey>,
        limits: TaskLimits,
    ) -> Result<BTreeSet<RecordKey>> {
        let mut seen = BTreeSet::new();
        let mut pending = roots;
        let mut edges = 0usize;
        while let Some(key) = pending.pop() {
            if !seen.insert(key.clone()) {
                continue;
            }
            if seen.len() > limits.records {
                return Err(Error::Limit("task records"));
            }
            let record = records
                .get(&key)
                .ok_or(Error::Integrity("missing task dependency"))?;
            let dependencies = record.dependencies();
            edges = edges
                .checked_add(dependencies.len())
                .ok_or(Error::Limit("task edges"))?;
            if edges > limits.edges {
                return Err(Error::Limit("task edges"));
            }
            pending.extend(dependencies);
        }
        Ok(seen)
    }
    pub fn record(&self, key: &RecordKey) -> Result<&TaskRecord> {
        self.records
            .get(key)
            .ok_or(Error::Integrity("missing selected record"))
    }
    /// These roots must be included in the same repository's pinned Git graph.
    /// Historical baseline refs not selected by a task are not inferred here;
    /// a selected TaskBaseline snapshot contributes its explicit inventory.
    pub fn object_roots(&self, source: &impl ArtifactSource) -> Result<Vec<ObjectId>> {
        let mut roots = BTreeSet::new();
        let mut add = |value: &str| -> Result<()> {
            let oid: ObjectId = value
                .parse()
                .map_err(|_| Error::Integrity("task Git object id"))?;
            if oid.algo() != alt_git_codec::HashAlgo::Sha1 {
                return Err(Error::Integrity("task Git object format"));
            }
            roots.insert(oid);
            if roots.len() > self.limits.edges {
                return Err(Error::Limit("task Git roots"));
            }
            Ok(())
        };
        for record in self.records.values() {
            match record {
                TaskRecord::Task(v) => {
                    add(&v.baseline.head)?;
                    add(&v.baseline.tree)?;
                    for oid in v.target_expected.values() {
                        add(oid)?;
                    }
                }
                TaskRecord::Candidate(v) => {
                    add(&v.tree)?;
                    for oid in v.target_expected.values() {
                        add(oid)?;
                    }
                }
                TaskRecord::Integration(v) => {
                    add(&v.commit)?;
                    add(&v.before)?;
                    add(&v.notes_after)?;
                    if let Some(oid) = &v.notes_before {
                        add(oid)?;
                    }
                }
                TaskRecord::TaskBaseline(v) => {
                    let bytes = crate::source::read_artifact(
                        source,
                        &v.snapshot,
                        self.limits.structured_bytes,
                    )?;
                    let snapshot: alt_delivery::Snapshot = alt_json::from_value(
                        crate::strict_json::decode(&bytes, self.limits.structured_bytes)?,
                    )?;
                    if snapshot.schema_version != 1 || snapshot.object_format != "sha1" {
                        return Err(Error::Integrity("task baseline snapshot format"));
                    }
                    for oid in snapshot.objects.keys() {
                        add(oid)?;
                    }
                }
                _ => {}
            }
        }
        Ok(roots.into_iter().collect())
    }
}
