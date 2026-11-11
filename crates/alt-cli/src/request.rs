//! Durable whole-command receipts. A pending request is never rerun merely
//! because no response was received. Operation completion is separate.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use alt_json::{Value, json};

use crate::daemon::{RequestId, Response};
use crate::machine::{Failure, hex};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

pub use alt_command_protocol::{Completion, Intent};

pub enum Claimed {
    Execute(Claim),
    Replay(Response),
}

/// Held outside the daemon store mutex, including while awaiting durability.
pub struct Claim {
    _lock: File,
    directory: PathBuf,
    intent: Intent,
}

pub fn parse_id(value: &str) -> Result<RequestId, Failure> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Failure::usage("request id must be 32 hexadecimal digits"));
    }
    let mut id = [0; 16];
    for (slot, pair) in id.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII hex"), 16)
            .expect("validated hex");
    }
    Ok(id)
}

pub fn new_id() -> std::io::Result<RequestId> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    alt_hash::blake3::hash(bytes).to_hex().to_string()
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// Caller supplies a deterministic binding (effective arguments, identity,
/// execution environment, repo/workspace instances). Only its digest persists.
pub fn claim(
    alt_dir: &Path,
    id: RequestId,
    repository: String,
    workspace: String,
    binding: &Value,
) -> Res<Claimed> {
    let request_id = hex(&id);
    let directory = alt_dir.join("requests").join(&request_id);
    std::fs::create_dir_all(&directory)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("lock"))?;
    #[cfg(unix)]
    {
        alt_os::sys::flock(&lock, alt_os::sys::LOCK_EX)?;
    }
    let binding_digest = digest(&alt_json::to_vec(binding)?);
    let intent_path = directory.join("intent.json");
    match std::fs::read(&intent_path) {
        Ok(bytes) => {
            let intent: Intent = alt_json::from_slice(&bytes)?;
            if intent.version != 1
                || intent.hash_algorithm != "blake3"
                || intent.request_id != request_id
            {
                let mut failure = Failure::unknown("invalid or unsupported request intent");
                failure.kind = "unsupported_record";
                return Err(failure.into());
            }
            if intent.binding_digest != binding_digest
                || intent.repository != repository
                || intent.workspace != workspace
            {
                return Err(Failure::refusal(
                    "request_mismatch",
                    "request id is bound to different arguments, identity, or workspace",
                )
                .into());
            }
            if let Some((_, response)) = read_completion(&directory, &intent)? {
                return Ok(Claimed::Replay(response));
            }
            let mut failure = Failure::unknown("request has no durable whole-command result");
            *failure.details = json!({"request_id": request_id, "state": "pending"});
            Err(failure.into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if directory.join("completed.json").exists() || directory.join("response.bin").exists()
            {
                return Err(
                    Failure::unknown("request intent is missing but result data exists").into(),
                );
            }
            let intent = Intent {
                version: 1,
                hash_algorithm: "blake3".into(),
                request_id,
                binding_digest,
                repository,
                workspace,
                started_ms: now_ms(),
            };
            durable_write(&intent_path, &alt_json::to_vec(&intent)?)?;
            Ok(Claimed::Execute(Claim {
                _lock: lock,
                directory,
                intent,
            }))
        }
        Err(error) => Err(error.into()),
    }
}

impl Claim {
    /// Call only after the command's data/log durability barrier succeeds.
    pub fn complete(self, response: &Response, operations: Vec<String>, outcome: &str) -> Res<()> {
        let length = response
            .stdout
            .len()
            .checked_add(response.stderr.len())
            .and_then(|size| size.checked_add(9));
        if length.is_none_or(|size| size > u32::MAX as usize) {
            durable_write(&self.directory.join("stdout.bin"), &response.stdout)?;
            durable_write(&self.directory.join("stderr.bin"), &response.stderr)?;
            let artifacts = json!({"stdout":{"path":"stdout.bin","bytes":response.stdout.len(),"blake3":digest(&response.stdout)},
                "stderr":{"path":"stderr.bin","bytes":response.stderr.len(),"blake3":digest(&response.stderr)},"exit_code":response.exit_code,"truncated":false});
            durable_write(
                &self.directory.join("output-artifacts.json"),
                &alt_json::to_vec(&artifacts)?,
            )?;
            let mut failure = Failure::unknown(
                "response exceeds the wire length limit; complete output is preserved in the request directory",
            );
            *failure.details = artifacts;
            return Err(failure.into());
        }
        let bytes = response.encode();
        durable_write(&self.directory.join("response.bin"), &bytes)?;
        let completion = Completion {
            version: 1,
            hash_algorithm: "blake3".into(),
            request_id: self.intent.request_id,
            binding_digest: self.intent.binding_digest,
            finished_ms: now_ms(),
            response_digest: digest(&bytes),
            response_bytes: bytes.len() as u64,
            operations,
            outcome: outcome.to_owned(),
        };
        durable_write(
            &self.directory.join("completed.json"),
            &alt_json::to_vec(&completion)?,
        )
    }
}

pub fn read_completion(directory: &Path, intent: &Intent) -> Res<Option<(Completion, Response)>> {
    let bytes = match std::fs::read(directory.join("completed.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let completion: Completion = alt_json::from_slice(&bytes)?;
    let bytes = std::fs::read(directory.join("response.bin"))?;
    let response = alt_command_protocol::validate_completion(intent, &completion, &bytes)
        .map_err(|error| Failure::unknown(error.to_string()))?;
    Ok(Some((completion, response)))
}

fn durable_write(path: &Path, bytes: &[u8]) -> Res<()> {
    let parent = path.parent().ok_or("missing state directory")?;
    let mut tmp = alt_os::temp::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    for directory in std::fs::canonicalize(parent)?.ancestors().take(3) {
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

/// Install an immutable repository identity without racing another caller.
pub fn repository_identity(alt_dir: &Path) -> Res<String> {
    let path = alt_dir.join("repository-id");
    match std::fs::read_to_string(&path) {
        Ok(id) => {
            parse_id(&id)?;
            Ok(id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = hex(&new_id()?);
            let mut tmp = alt_os::temp::NamedTempFile::new_in(alt_dir)?;
            tmp.write_all(id.as_bytes())?;
            tmp.as_file().sync_all()?;
            match tmp.persist_noclobber(&path) {
                Ok(_) => File::open(alt_dir)?.sync_all()?,
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            repository_identity(alt_dir)
        }
        Err(error) => Err(error.into()),
    }
}

/// Persist caller possession before a request can cross the socket boundary.
/// This is separate from the execution claim: it never blocks the daemon.
pub fn record_submission(alt_dir: &Path, id: RequestId, binding: &Value) -> Res<()> {
    let directory = alt_dir.join("submissions");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", hex(&id)));
    let bytes = alt_json::to_vec(
        &json!({"version":1, "request_id":hex(&id), "binding_digest":digest(&alt_json::to_vec(binding)?)}),
    )?;
    let mut tmp = alt_os::temp::NamedTempFile::new_in(&directory)?;
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    match tmp.persist_noclobber(&path) {
        Ok(_) => {
            File::open(&directory)?.sync_all()?;
            File::open(alt_dir)?.sync_all()?;
        }
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(path)? != bytes {
                return Err(Failure::refusal(
                    "request_mismatch",
                    "request id was submitted with different arguments or scope",
                )
                .into());
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub fn query(
    store: &crate::native::Store,
    op: &crate::cli::RequestOp,
    json_mode: bool,
    out: &mut impl Write,
) -> Res<()> {
    use crate::cli::RequestOp;
    let describe = |id: &str, include_result: bool| -> Res<Value> {
        let id = hex(&parse_id(id)?);
        let directory = store.alt_dir().join("requests").join(&id);
        let bytes = match std::fs::read(directory.join("intent.json")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if directory.exists()
                    || store
                        .alt_dir()
                        .join("submissions")
                        .join(format!("{id}.json"))
                        .exists()
                {
                    return Ok(
                        json!({"request_id":id,"state":"submitted","outcome":"unknown", "operation_ids":store.request_operations(&id)?,
                        "next_action":"No execution intent is visible yet. Query again or retry only this same request id and identical arguments."}),
                    );
                }
                return Err(Failure::refusal(
                    "request_not_found",
                    "no durable request intent or submission was found",
                )
                .into());
            }
            Err(error) => return Err(error.into()),
        };
        let intent: Intent = alt_json::from_slice(&bytes)
            .map_err(|error| Failure::unknown(format!("cannot verify request intent: {error}")))?;
        if intent.version != 1 || intent.hash_algorithm != "blake3" || intent.request_id != id {
            return Err(Failure::unknown("unsupported request intent").into());
        }
        let completion = read_completion(&directory, &intent)
            .map_err(|error| Failure::unknown(format!("cannot verify request result: {error}")))?;
        let mut value = json!({"request_id": id, "repository": intent.repository,
            "workspace":intent.workspace,"state":if completion.is_some(){"completed"}else{"pending"},
            "operation_ids":store.request_operations(&id)?, "started_ms":intent.started_ms});
        if let Some((completion, response)) = completion {
            value["recorded_operation_ids"] = json!(completion.operations);
            value["finished_ms"] = json!(completion.finished_ms);
            value["exit_code"] = json!(response.exit_code);
            value["outcome"] = json!(completion.outcome);
            value["response_digest"] = json!(completion.response_digest);
            value["response_bytes"] = json!(completion.response_bytes);
            if include_result {
                value["stdout"] = crate::machine::bytes_value(&response.stdout);
                value["stderr"] = crate::machine::bytes_value(&response.stderr);
            }
        } else {
            if let Ok(bytes) = std::fs::read(directory.join("output-artifacts.json")) {
                value["output_artifacts"] = alt_json::from_slice(&bytes)?;
            }
            value["outcome"] = json!("unknown");
            value["next_action"] = json!(
                "Inspect attributed operations; recover a prepared local operation explicitly. Do not rerun unknown remaining command steps."
            );
        }
        Ok(value)
    };
    let value = match op {
        RequestOp::Status { id } => json!({"request":describe(id, false)?}),
        RequestOp::Result { id } => json!({"request":describe(id, true)?}),
        RequestOp::Pending => {
            let mut requests = Vec::new();
            let mut ids = std::collections::BTreeSet::new();
            for name in ["requests", "submissions"] {
                let directory = store.alt_dir().join(name);
                if !directory.exists() {
                    continue;
                }
                for entry in std::fs::read_dir(directory)? {
                    let name = entry?.file_name();
                    let Some(name) = name.to_str() else {
                        continue;
                    };
                    let id = name.strip_suffix(".json").unwrap_or(name);
                    if parse_id(id).is_ok() {
                        ids.insert(id.to_owned());
                    }
                }
            }
            for id in ids {
                let value = describe(&id, false)?;
                if value["state"] != "completed" {
                    requests.push(value);
                }
            }
            json!({"requests":requests})
        }
    };
    if json_mode {
        let mut value = value;
        value["schema_version"] = json!(1);
        writeln!(out, "{value}")?;
    } else {
        writeln!(out, "{}", alt_json::to_string_pretty(&value)?)?;
    }
    Ok(())
}

/// Reconciliation may run while holding the store mutex, so it must never
/// wait for another request whose executor could be waiting for that mutex.
/// The returned guard does not change the original intent or result files.
pub(crate) fn try_lock_existing(alt_dir: &Path, request: &str) -> Res<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let id = parse_id(request)?;
    if hex(&id) != request {
        return Err(
            Failure::refusal("request_mismatch", "original request id must be canonical").into(),
        );
    }
    let path = alt_dir.join("requests").join(request).join("lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
        .open(path)?;
    if !lock.metadata()?.is_file() {
        return Err(Failure::refusal(
            "request_mismatch",
            "original request lock is not a regular file",
        )
        .into());
    }
    if let Err(error) = alt_os::sys::flock(&lock, alt_os::sys::LOCK_EX | alt_os::sys::LOCK_NB) {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Err(Failure::refusal(
                "request_active",
                "original request is still running; inspect it after it finishes",
            )
            .into());
        }
        return Err(error.into());
    }
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acquire(root: &Path, binding: Value) -> Res<Claimed> {
        claim(root, [7; 16], "repo".into(), "workspace".into(), &binding)
    }

    #[test]
    fn completed_failure_replays_the_entire_response() {
        let root = alt_os::temp::tempdir().unwrap();
        let binding = json!({"command": "commit", "arguments": ["日本語"]});
        let Claimed::Execute(claim) = acquire(root.path(), binding.clone()).unwrap() else {
            panic!("new request must execute");
        };
        let response = Response {
            exit_code: 1,
            stdout: Vec::new(),
            stderr: "拒否\n".repeat(200_000).into_bytes(),
        };
        claim
            .complete(&response, Vec::new(), "not_applied")
            .unwrap();
        let Claimed::Replay(replayed) = acquire(root.path(), binding).unwrap() else {
            panic!("completed request must replay");
        };
        assert_eq!(response, replayed);
        let error = acquire(root.path(), json!({"command": "add"}))
            .err()
            .unwrap();
        assert_eq!(
            error.downcast_ref::<Failure>().unwrap().kind,
            "request_mismatch"
        );
    }

    #[test]
    fn interrupted_intent_is_unknown_even_if_response_bytes_exist() {
        let root = alt_os::temp::tempdir().unwrap();
        let binding = json!({"command": "switch"});
        let Claimed::Execute(claim) = acquire(root.path(), binding.clone()).unwrap() else {
            panic!("new request must execute");
        };
        durable_write(&claim.directory.join("response.bin"), b"partial result").unwrap();
        drop(claim);
        let error = acquire(root.path(), binding).err().unwrap();
        let failure = error.downcast_ref::<Failure>().unwrap();
        assert_eq!(failure.outcome, "unknown");
        assert_eq!(failure.exit_code, 75);
    }

    #[test]
    fn corrupt_completed_response_is_never_reexecuted() {
        let root = alt_os::temp::tempdir().unwrap();
        let binding = json!({"command": "add"});
        let Claimed::Execute(claim) = acquire(root.path(), binding.clone()).unwrap() else {
            panic!("new request must execute");
        };
        let directory = claim.directory.clone();
        claim
            .complete(
                &Response {
                    exit_code: 0,
                    stdout: b"ok".to_vec(),
                    stderr: Vec::new(),
                },
                Vec::new(),
                "applied",
            )
            .unwrap();
        std::fs::write(directory.join("response.bin"), b"torn").unwrap();
        let error = acquire(root.path(), binding).err().unwrap();
        assert_eq!(error.downcast_ref::<Failure>().unwrap().outcome, "unknown");
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;

    alt_json_testkit::module_read_tests!("state-request"; Intent: Intent, Completion: Completion);
}
