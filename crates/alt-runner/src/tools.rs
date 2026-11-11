use crate::{Error, Result, capture_file};
use alt_input::ToolIdentity;
use alt_task::{Artifact, Termination};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct ToolBinding {
    pub name: String,
    /// Absolute installed tool or relative executable from the selected input.
    pub path: PathBuf,
    /// Empty only for an input script, whose version is its content identity.
    pub version_args: Vec<String>,
}

fn resolve(root: &Path, binding: &ToolBinding) -> Result<PathBuf> {
    if binding.name.is_empty() {
        return Err(Error::Invalid("empty tool name".into()));
    }
    let path = if binding.path.is_absolute() {
        binding.path.clone()
    } else {
        alt_input::checked_path(binding.path.as_os_str().as_encoded_bytes())?;
        root.join(&binding.path)
    };
    let path = fs::canonicalize(path)?;
    if path.to_str().is_none() {
        return Err(Error::Invalid("non-UTF-8 tool path".into()));
    }
    if !binding.path.is_absolute() && !path.starts_with(fs::canonicalize(root)?) {
        return Err(Error::Invalid("input tool escapes root".into()));
    }
    let metadata = path.metadata()?;
    use std::os::unix::fs::PermissionsExt;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(Error::Invalid("tool is not executable".into()));
    }
    Ok(path)
}

pub(crate) struct ProbeCapture<'a> {
    pub control: &'a Path,
    pub captures: &'a mut BTreeMap<String, String>,
    pub artifacts: &'a mut BTreeMap<String, Artifact>,
    pub phase: &'a str,
}

fn version(
    root: &Path,
    path: &Path,
    binding: &ToolBinding,
    environment: &BTreeMap<String, String>,
    digest: &str,
    capture: Option<ProbeCapture<'_>>,
) -> Result<String> {
    if binding.version_args.is_empty() {
        if binding.path.is_absolute() {
            return Err(Error::Invalid(
                "installed tool needs a version probe".into(),
            ));
        }
        return Ok(format!("input:{digest}"));
    }
    use std::os::unix::process::CommandExt;
    let mut stdout = alt_os::temp::NamedTempFile::new()?;
    let mut stderr = alt_os::temp::NamedTempFile::new()?;
    let mut child = Command::new(path)
        .args(&binding.version_args)
        .current_dir(root)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(stdout.as_file().try_clone()?)
        .stderr(stderr.as_file().try_clone()?)
        .process_group(0)
        .spawn()?;
    let (status, _) = crate::process::wait(&mut child, Duration::from_secs(5))?;
    if let Some(capture) = capture {
        for (stream, file) in [("stdout", &stdout), ("stderr", &stderr)] {
            let artifact = capture_file(capture.control, file.path(), capture.captures)?;
            capture.artifacts.insert(
                format!("tool-probe:{}:{}:{stream}", capture.phase, binding.name),
                artifact,
            );
        }
        let bytes = alt_json::to_vec(&status)?;
        let artifact = crate::record_bytes(capture.control, &bytes, capture.captures)?;
        capture.artifacts.insert(
            format!("tool-probe:{}:{}:termination", capture.phase, binding.name),
            artifact,
        );
    }
    if status != (Termination::Exit { code: 0 }) {
        return Err(Error::Invalid(format!("version probe failed: {status:?}")));
    }
    if stdout.as_file().metadata()?.len() + stderr.as_file().metadata()?.len() > 65536 {
        return Err(Error::Invalid("version probe output exceeds 64 KiB".into()));
    }
    use std::io::{Read, Seek};
    stdout.rewind()?;
    stderr.rewind()?;
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        stderr.read_to_end(&mut bytes)?;
    }
    let value =
        String::from_utf8(bytes).map_err(|_| Error::Invalid("non-UTF-8 tool version".into()))?;
    if value.trim().is_empty() {
        return Err(Error::Invalid("empty tool version".into()));
    }
    Ok(value.trim().into())
}

pub fn probe(
    root: &Path,
    binding: &ToolBinding,
    environment: &BTreeMap<String, String>,
) -> Result<ToolIdentity> {
    let path = resolve(root, binding)?;
    let bytes = fs::read(&path)?;
    let digest = alt_hash::blake3::hash(&bytes).to_hex().to_string();
    let identity = ToolIdentity {
        name: binding.name.clone(),
        version: version(root, &path, binding, environment, &digest, None)?,
        binary_blake3: Some(digest.clone()),
    };
    let current = fs::read(resolve(root, binding)?)?;
    if alt_hash::blake3::hash(&current).to_hex().as_str() != digest {
        return Err(Error::Invalid("tool changed during version probe".into()));
    }
    Ok(identity)
}

pub(crate) fn capture_probe(
    root: &Path,
    binding: &ToolBinding,
    environment: &BTreeMap<String, String>,
    expected: Option<&str>,
    capture: ProbeCapture<'_>,
) -> Result<(ToolIdentity, Artifact, PathBuf)> {
    let path = resolve(root, binding)?;
    let artifact = capture_file(capture.control, &path, capture.captures)?;
    if expected != Some(artifact.blake3.as_str()) {
        return Err(Error::Invalid(
            "tool binary differs from the candidate; version probe was not executed".into(),
        ));
    }
    let control = capture.control;
    let identity = ToolIdentity {
        name: binding.name.clone(),
        version: version(
            root,
            &path,
            binding,
            environment,
            &artifact.blake3,
            Some(ProbeCapture {
                control,
                captures: &mut *capture.captures,
                artifacts: &mut *capture.artifacts,
                phase: capture.phase,
            }),
        )?,
        binary_blake3: Some(artifact.blake3.clone()),
    };
    if capture_file(control, &resolve(root, binding)?, capture.captures)? != artifact {
        return Err(Error::Invalid("tool changed during version probe".into()));
    }
    Ok((identity, artifact, path))
}

pub(crate) fn observe_external(
    root: &Path,
    control: &Path,
    context: &alt_input::Context,
    bindings: &BTreeMap<String, PathBuf>,
    captures: &mut BTreeMap<String, String>,
    artifacts: &mut BTreeMap<String, Artifact>,
    phase: &str,
) -> Result<Vec<alt_input::ExternalDependency>> {
    let probes: BTreeMap<String, Vec<String>> = context
        .validation
        .get("external_probes")
        .map(|value| alt_json::from_value(value.clone()))
        .transpose()?
        .unwrap_or_default();
    let mut observed = Vec::new();
    for expected in &context.external {
        let mut dependency = expected.clone();
        dependency.version = None;
        if let Some(argv) = probes.get(&expected.provider)
            && let Some(program) = argv.first().and_then(|name| bindings.get(name))
        {
            use std::os::unix::process::CommandExt;
            let stdout = alt_os::temp::NamedTempFile::new_in(control)?;
            let stderr = alt_os::temp::NamedTempFile::new_in(control)?;
            let mut child = Command::new(program)
                .args(&argv[1..])
                .current_dir(root)
                .env_clear()
                .envs(&context.environment.values)
                .stdin(Stdio::null())
                .stdout(stdout.as_file().try_clone()?)
                .stderr(stderr.as_file().try_clone()?)
                .process_group(0)
                .spawn()?;
            let (termination, _) = crate::process::wait(&mut child, Duration::from_secs(10))?;
            let output = capture_file(control, stdout.path(), captures)?;
            let diagnostics = capture_file(control, stderr.path(), captures)?;
            if termination == (Termination::Exit { code: 0 })
                && output.bytes <= 65536
                && let Ok(value) = fs::read_to_string(stdout.path())
                && !value.trim().is_empty()
            {
                dependency.version = Some(value.trim().into());
            }
            artifacts.insert(
                format!("external:{phase}:{}:stdout", expected.provider),
                output,
            );
            artifacts.insert(
                format!("external:{phase}:{}:stderr", expected.provider),
                diagnostics,
            );
            let record = alt_json::to_vec(
                &alt_json::json!({"provider":expected.provider,"argv":argv,"termination":termination,"observed_version":dependency.version}),
            )?;
            artifacts.insert(
                format!("external:{phase}:{}:observation", expected.provider),
                crate::record_bytes(control, &record, captures)?,
            );
        }
        observed.push(dependency);
    }
    Ok(observed)
}
