//! Observe validation against materialized inputs. Transport and VCS authority
//! remain with the caller; a serialized report grants no import authority.
mod inputs;
mod process;
mod tools;

use alt_input::{Context, Manifest};
use alt_task::{Artifact, Evidence, Provenance, Termination};
pub use inputs::{InputObservation, materialize, observe_inputs, output_roots};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};
pub use tools::{ToolBinding, probe};

pub const CONTROL: &str = ".alt-runner";

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    Input(alt_input::Error),
    Invalid(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "runner I/O: {e}"),
            Self::Json(e) => write!(f, "runner encoding: {e}"),
            Self::Input(e) => std::fmt::Display::fmt(e, f),
            Self::Invalid(v) => write!(f, "runner precondition: {v}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Input(e) => e.source(),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<alt_input::Error> for Error {
    fn from(e: alt_input::Error) -> Self {
        Self::Input(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

pub fn host_identity() -> Result<String> {
    let mut name = [0u8; 256];
    if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let end = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len());
    let name = std::str::from_utf8(&name[..end])
        .map_err(|_| Error::Invalid("non-UTF-8 host identity".into()))?;
    Ok(format!(
        "{}/{}/{name}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub schema_version: u32,
    pub candidate: String,
    pub nonce: String,
    pub manifest: Manifest,
    pub tools: Vec<ToolBinding>,
    pub timeout_ms: u64,
    pub runner: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema_version: u32,
    pub nonce: String,
    pub evidence: Evidence,
    pub before: InputObservation,
    pub after: InputObservation,
    pub commands: Vec<process::CommandObservation>,
    pub problems: Vec<String>,
    /// Artifact digest -> relative path inside the control directory. Consumers
    /// verify every byte before publishing into their private evidence store.
    pub captures: BTreeMap<String, String>,
}

pub(crate) fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn record_bytes(
    control: &Path,
    bytes: &[u8],
    captures: &mut BTreeMap<String, String>,
) -> Result<Artifact> {
    let artifact = Artifact {
        blake3: blake3::hash(bytes).to_hex().to_string(),
        bytes: bytes.len() as u64,
    };
    let path = format!("captures/{}", artifact.blake3);
    fs::write(control.join(&path), bytes)?;
    captures.insert(artifact.blake3.clone(), path);
    Ok(artifact)
}

pub(crate) fn capture_file(
    control: &Path,
    path: &Path,
    captures: &mut BTreeMap<String, String>,
) -> Result<Artifact> {
    use std::io::Write;
    let path = fs::canonicalize(path)?;
    let root = alt_input::Root::open(
        path.parent()
            .ok_or_else(|| Error::Invalid("binary has no parent".into()))?,
    )?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Invalid("non-UTF-8 artifact path".into()))?;
    let mut file = tempfile::NamedTempFile::new_in(control.join("captures"))?;
    let node = root.copy(name, file.as_file_mut())?;
    file.flush()?;
    let relative = format!("captures/{}", node.content);
    let destination = control.join(&relative);
    if destination.exists() {
        // Never trust a preexisting capture just because its name is a digest.
        let bytes = fs::read(&destination)?;
        if blake3::hash(&bytes).to_hex().as_str() != node.content || bytes.len() as u64 != node.len
        {
            return Err(Error::Invalid(
                "capture digest collision or corruption".into(),
            ));
        }
    } else {
        file.persist_noclobber(destination)
            .map_err(|error| error.error)?;
    }
    captures.insert(node.content.clone(), relative);
    Ok(Artifact {
        blake3: node.content,
        bytes: node.len,
    })
}

/// The caller creates CONTROL before launching this observer. Each nonce has
/// one fresh result directory and is never executed twice by this entrypoint.
pub fn execute(root: &Path, spec: &Spec, authentication_key: &[u8; 32]) -> Result<Report> {
    if spec.schema_version != 1
        || spec.nonce.len() != 32
        || !spec.nonce.bytes().all(|b| b.is_ascii_hexdigit())
        || spec.timeout_ms == 0
        || spec.runner.is_empty()
    {
        return Err(Error::Invalid("invalid observer spec".into()));
    }
    spec.manifest.validate()?;
    alt_input::check_execution_platform(root)?;
    let outputs = output_roots(&spec.manifest)?;
    let control = root.join(CONTROL);
    fs::create_dir_all(&control)?;
    fs::File::open(root)?.sync_all()?;
    // An exclusive marker survives crashes. Recovery inspects it; it cannot
    // turn an uncertain execution into an automatic second invocation.
    let mut marker = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(control.join("started.json"))?;
    use std::io::Write;
    serde_json::to_writer(
        &mut marker,
        &serde_json::json!({"nonce":spec.nonce,"pid":std::process::id(),"started_unix_ns":now_ns()}),
    )?;
    marker.write_all(b"\n")?;
    marker.sync_all()?;
    fs::File::open(&control)?.sync_all()?;
    fs::create_dir(control.join("captures"))?;
    inputs::restore_root_auxiliary(root, &spec.manifest)?;
    inputs::prepare_outputs(root, &outputs)?;
    let started = now_ns();
    let mut captures = BTreeMap::new();
    let mut artifacts = BTreeMap::new();
    let mut problems = Vec::new();
    let mut actual: Context = spec.manifest.context.clone();
    let mut binaries = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    actual.tools.clear();
    for binding in &spec.tools {
        if bindings.contains_key(&binding.name) {
            return Err(Error::Invalid("duplicate tool binding".into()));
        }
        match tools::capture_probe(
            root,
            binding,
            &actual.environment.values,
            spec.manifest
                .context
                .tools
                .iter()
                .find(|tool| tool.name == binding.name)
                .and_then(|tool| tool.binary_blake3.as_deref()),
            tools::ProbeCapture {
                control: &control,
                captures: &mut captures,
                artifacts: &mut artifacts,
                phase: "before",
            },
        ) {
            Ok((identity, artifact, path)) => {
                binaries.insert(binding.name.clone(), artifact);
                bindings.insert(binding.name.clone(), path);
                actual.tools.push(identity);
            }
            Err(error) => problems.push(format!("tool {}: {error}", binding.name)),
        }
    }
    actual.external = tools::observe_external(
        root,
        &control,
        &spec.manifest.context,
        &bindings,
        &mut captures,
        &mut artifacts,
        "before",
    )?;
    actual.tools.sort_by(|a, b| a.name.cmp(&b.name));
    let mut expected = spec.manifest.context.clone();
    expected.tools.sort_by(|a, b| a.name.cmp(&b.name));
    if actual != expected {
        problems.push("execution_context_mismatch".into());
    }
    problems.extend(actual.uncertainty());
    // Preserve declared ordering in the report, since it is part of identity.
    if actual == expected {
        actual = spec.manifest.context.clone();
    }
    let before = observe_inputs(root, &spec.manifest, &outputs);
    let manifest_id = spec.manifest.id()?;
    if before.identity.as_deref() != Some(&manifest_id) {
        problems.push("inputs_before_mismatch".into());
    }
    let stdout_path = control.join("stdout.bin");
    let stderr_path = control.join("stderr.bin");
    fs::File::create(&stdout_path)?;
    fs::File::create(&stderr_path)?;
    let (termination, commands) = if problems.is_empty() {
        process::run(
            root,
            &control,
            &actual,
            &bindings,
            spec.timeout_ms,
            &mut problems,
        )?
    } else {
        (Termination::Unknown, Vec::new())
    };
    for binding in &spec.tools {
        match tools::capture_probe(
            root,
            binding,
            &actual.environment.values,
            spec.manifest
                .context
                .tools
                .iter()
                .find(|tool| tool.name == binding.name)
                .and_then(|tool| tool.binary_blake3.as_deref()),
            tools::ProbeCapture {
                control: &control,
                captures: &mut captures,
                artifacts: &mut artifacts,
                phase: "after",
            },
        ) {
            Ok((identity, artifact, _))
                if binaries.get(&binding.name) == Some(&artifact)
                    && actual.tools.iter().any(|tool| tool == &identity) => {}
            Ok(_) => problems.push(format!("tool_changed:{}", binding.name)),
            Err(error) => problems.push(format!("tool_after_unknown:{}:{error}", binding.name)),
        }
    }
    let external_after = tools::observe_external(
        root,
        &control,
        &spec.manifest.context,
        &bindings,
        &mut captures,
        &mut artifacts,
        "after",
    )?;
    if external_after != actual.external {
        problems.push("external_dependency_changed".into());
    }
    let after = observe_inputs(root, &spec.manifest, &outputs);
    if after.identity.as_deref() != Some(&manifest_id) {
        problems.push("inputs_after_mismatch".into());
    }
    let stdout = capture_file(&control, &stdout_path, &mut captures)?;
    let stderr = capture_file(&control, &stderr_path, &mut captures)?;
    artifacts.insert(
        "input_observations".into(),
        record_bytes(
            &control,
            &serde_json::to_vec(&(&before, &after))?,
            &mut captures,
        )?,
    );
    artifacts.insert(
        "command_observations".into(),
        record_bytes(&control, &serde_json::to_vec(&commands)?, &mut captures)?,
    );
    let output_inventory =
        inputs::capture_outputs(root, &control, &outputs, &mut captures, &mut artifacts)?;
    artifacts.insert(
        "output_inventory".into(),
        record_bytes(
            &control,
            &serde_json::to_vec(&output_inventory)?,
            &mut captures,
        )?,
    );
    let report = Report {
        schema_version: 1,
        nonce: spec.nonce.clone(),
        before: before.clone(),
        after: after.clone(),
        commands,
        problems: problems.clone(),
        captures,
        evidence: Evidence {
            schema_version: 1,
            candidate: spec.candidate.clone(),
            manifest: manifest_id,
            before: before.identity,
            after: if problems.is_empty() {
                after.identity
            } else {
                None
            },
            runner: format!("{} [{}]", spec.runner, host_identity()?),
            actual_context: actual,
            binaries,
            started_unix_ns: started,
            finished_unix_ns: now_ns(),
            termination,
            stdout: Some(stdout),
            stderr: Some(stderr),
            artifacts,
            provenance: Provenance::Observed,
            original_report: None,
            creator_request: String::new(),
        },
    };
    let mut file = tempfile::NamedTempFile::new_in(&control)?;
    let report_bytes = serde_json::to_vec(&report)?;
    file.write_all(&report_bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(control.join("report.json"))
        .map_err(|error| error.error)?;
    let mut mac = tempfile::NamedTempFile::new_in(&control)?;
    mac.write_all(
        blake3::keyed_hash(authentication_key, &report_bytes)
            .to_hex()
            .as_bytes(),
    )?;
    mac.as_file().sync_all()?;
    mac.persist_noclobber(control.join("report.mac"))
        .map_err(|error| error.error)?;
    fs::File::open(&control)?.sync_all()?;
    Ok(report)
}

#[cfg(test)]
mod tests;
