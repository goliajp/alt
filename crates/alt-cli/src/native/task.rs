//! Task records live outside the Git ODB. Request completion, not the presence
//! of a metadata file, determines whether a creation command finished.
use super::*;
use alt_task::{Artifact, Candidate, Event, Evidence, Record, Scope, Task, TaskState};
use serde_json::json;
use std::collections::BTreeMap;

mod candidate;
mod commands;
mod evidence;
mod run;

pub(super) const TASK_OPERATION: u8 = 0x34;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskOperation {
    pub version: u32,
    pub workspace: recovery::WorkspaceIdentity,
    pub request_id: Option<String>,
    pub task_id: String,
    pub action: String,
    pub record_id: String,
}

fn refused(message: impl Into<String>) -> Box<dyn std::error::Error> {
    crate::machine::Failure::refusal("precondition_failed", message).into()
}
fn emit(out: &mut impl Write, json_mode: bool, fields: serde_json::Value) -> Res<()> {
    if json_mode {
        let mut fields = fields;
        fields["schema_version"] = json!(1);
        serde_json::to_writer(&mut *out, &fields)?;
        writeln!(out)?;
    } else {
        writeln!(out, "{}", serde_json::to_string_pretty(&fields)?)?;
    }
    Ok(())
}

struct IndexTree<'a> {
    odb: &'a NativeOdb,
    entries: BTreeMap<String, (u32, ObjectId)>,
}
impl IndexTree<'_> {
    fn bytes(&self, path: &str) -> alt_input::Result<Vec<u8>> {
        let (_, oid) = self
            .entries
            .get(path)
            .ok_or_else(|| alt_input::Error::Integrity("index path missing".into()))?;
        let object = self
            .odb
            .get(oid)
            .map_err(|error| alt_input::Error::Integrity(error.to_string()))?
            .ok_or_else(|| alt_input::Error::Integrity("index blob missing".into()))?;
        if object.kind != ObjectKind::Blob {
            return Err(alt_input::Error::Unsupported(
                "non-blob source input".into(),
            ));
        }
        Ok(object.data)
    }
}
impl alt_input::Tree for IndexTree<'_> {
    fn inspect(&self, path: &str) -> alt_input::Result<alt_input::Node> {
        let (mode, _) = self
            .entries
            .get(path)
            .ok_or_else(|| alt_input::Error::Integrity("index path missing".into()))?;
        let bytes = self.bytes(path)?;
        let kind = match mode {
            0o100644 | 0o100755 => alt_input::EntryKind::File,
            0o120000 => alt_input::EntryKind::Symlink,
            _ => {
                return Err(alt_input::Error::Unsupported(format!(
                    "unsupported source mode {mode:o}: {path}"
                )));
            }
        };
        Ok(alt_input::Node {
            link_target: if kind == alt_input::EntryKind::Symlink {
                Some(alt_input::hex(&bytes))
            } else {
                None
            },
            kind,
            executable: *mode == 0o100755,
            len: bytes.len() as u64,
            content: blake3::hash(&bytes).to_hex().to_string(),
        })
    }
    fn children(&self, _: &str) -> alt_input::Result<Vec<String>> {
        Err(alt_input::Error::Unsupported(
            "index source is an explicit file selection".into(),
        ))
    }
}

impl NativeRepo<'_> {
    fn task_store(&self) -> Res<alt_task::Store> {
        let directory = self.store.alt_dir.join("task-data");
        if !directory.is_dir() {
            return Err(refused("repository has no task records"));
        }
        Ok(alt_task::Store::open(&directory)?)
    }
    fn task_request(&self) -> Res<String> {
        self.request_id
            .map(|id| crate::machine::hex(&id))
            .ok_or_else(|| refused("task writes require a whole-command request identity"))
    }
    fn task_scope(&self) -> Res<Scope> {
        Ok(Scope {
            repository: std::fs::read_to_string(self.store.alt_dir.join("repository-id"))?,
            workspace: std::fs::read_to_string(self.index_path.with_file_name("workspace-id"))?,
        })
    }
    fn evidence_request_complete(&self, id: &str) -> Res<bool> {
        self.record_request_complete(id, false)
    }
    fn task_request_complete(&self, id: &str) -> Res<bool> {
        self.record_request_complete(id, true)
    }
    fn record_request_complete(&self, id: &str, require_success: bool) -> Res<bool> {
        crate::request::parse_id(id)?;
        let directory = self.store.alt_dir.join("requests").join(id);
        let bytes = match std::fs::read(directory.join("intent.json")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let intent: crate::request::Intent = serde_json::from_slice(&bytes)?;
        Ok(
            crate::request::read_completion(&directory, &intent)?.is_some_and(
                |(completion, response)| {
                    completion.outcome == "applied" && (!require_success || response.exit_code == 0)
                },
            ),
        )
    }
    fn require_task(&self, store: &alt_task::Store, id: &str, active: bool) -> Res<Task> {
        let task: Task = store.load(id)?;
        if task.scope != self.task_scope()? {
            return Err(refused(
                "task belongs to a different repository/workspace instance",
            ));
        }
        if !self.task_request_complete(&task.creator_request)? {
            return Err(crate::machine::Failure::unknown("task creation has no successful durable request completion; inspect its request and operation").into());
        }
        if active {
            for (_, event) in store.list::<Event>()? {
                if event.task == id {
                    if !self.task_request_complete(&event.creator_request)? {
                        return Err(crate::machine::Failure::unknown(
                            "task state event has no successful durable request completion",
                        )
                        .into());
                    }
                    return Err(refused(
                        "task has a terminal state event; inspect its history",
                    ));
                }
            }
        }
        Ok(task)
    }
    fn task_state(&self, store: &alt_task::Store, id: &str, task: &Task) -> Res<&'static str> {
        if !self.task_request_complete(&task.creator_request)? {
            return Ok("unknown");
        }
        let events: Vec<_> = store
            .list::<Event>()?
            .into_iter()
            .filter(|(_, event)| event.task == id)
            .collect();
        if events.len() > 1 {
            return Ok("unknown");
        }
        if let Some((_, event)) = events.first() {
            if !self.task_request_complete(&event.creator_request)? {
                return Ok("unknown");
            }
            return Ok(match event.state {
                TaskState::Abandoned => "abandoned",
                TaskState::Integrated => "integrated",
            });
        }
        Ok("active")
    }
    fn task_operation(&mut self, task: &str, action: &str, record: &str) -> Res<String> {
        let operation = TaskOperation {
            version: 1,
            workspace: self.workspace_identity()?,
            request_id: Some(self.task_request()?),
            task_id: task.into(),
            action: action.into(),
            record_id: record.into(),
        };
        let op = self.store.refs.record_op(
            &self.id.actor(action),
            now_ms(),
            &recovery::encode(TASK_OPERATION, &operation)?,
        )?;
        Ok(op.to_string())
    }
    fn task_targets(&self, expected: &BTreeMap<String, String>) -> Res<BTreeMap<String, String>> {
        expected
            .keys()
            .map(|name| {
                Ok((
                    name.clone(),
                    self.store
                        .refs
                        .resolve(name)?
                        .map(|oid| oid.to_string())
                        .unwrap_or_default(),
                ))
            })
            .collect::<Res<_>>()
    }
    fn task_index(&self) -> Res<IndexTree<'_>> {
        let index = self.index()?;
        let mut entries = BTreeMap::new();
        for entry in index.entries {
            if entry.stage() != 0 || entry.skip_worktree() || entry.intent_to_add() {
                return Err(refused(
                    "task input requires a resolved, complete index without sparse or intent-to-add entries",
                ));
            }
            let path = alt_input::checked_path(entry.path.as_slice())?.to_owned();
            if !matches!(entry.mode, 0o100644 | 0o100755 | 0o120000) {
                return Err(refused(format!(
                    "unsupported task source mode {:o}: {path}",
                    entry.mode
                )));
            }
            entries.insert(path, (entry.mode, entry.oid));
        }
        Ok(IndexTree {
            odb: &self.store.odb,
            entries,
        })
    }
    fn task_manifest(&self, task: &Task) -> Res<alt_input::Manifest> {
        alt_input::check_execution_platform(&self.root)?;
        let source = self.task_index()?;
        let auxiliary = alt_input::Root::open(&self.root)?;
        let plan = alt_input::InputPlan {
            source: source.entries.keys().cloned().collect(),
            auxiliary: task.spec.auxiliary.clone(),
            exclusions: task.spec.exclusions.clone(),
        };
        Ok(alt_input::scan(
            &source,
            &auxiliary,
            plan,
            task.spec.validation.clone(),
        )?)
    }
    fn task_check_paths(&self, task: &Task) -> Res<()> {
        let baseline = self.commit_entries(ObjectId::from_hex(task.baseline.head.as_bytes())?)?;
        let before: BTreeMap<_, _> = baseline
            .into_iter()
            .map(|entry| (entry.path.as_bytes().to_vec(), (entry.mode, entry.oid)))
            .collect();
        let after = self.task_index()?;
        let paths: std::collections::BTreeSet<_> = before
            .keys()
            .map(|path| alt_input::checked_path(path).map(str::to_owned))
            .collect::<alt_input::Result<_>>()?;
        for path in paths.into_iter().chain(after.entries.keys().cloned()) {
            if before.get(path.as_bytes()) != after.entries.get(&path) && !task.spec.allows(&path) {
                return Err(refused(format!("change is outside task authority: {path}")));
            }
        }
        Ok(())
    }
}
