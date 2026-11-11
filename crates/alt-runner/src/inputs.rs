use crate::{CONTROL, Error, Result, capture_file};
use alt_input::{EntryKind, Manifest, Node, Root, Tree};
use alt_task::Artifact;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

fn covers(prefix: &str, path: &str) -> bool {
    prefix == path
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The canonical sync tool's `--also` accepts directories. A top-level ignored
/// auxiliary file travels in this typed bundle instead; source files never do.
pub(crate) fn restore_root_auxiliary(root: &Path, manifest: &Manifest) -> Result<()> {
    for input in &manifest.plan.auxiliary {
        if input.path.contains('/') {
            continue;
        }
        let entry = manifest
            .entries
            .iter()
            .find(|entry| entry.path == input.path);
        let Some(entry) = entry else {
            continue;
        }; // explicitly excluded root
        if entry.node.kind == EntryKind::Directory {
            continue;
        }
        let reader = Root::open(root)?;
        match fs::symlink_metadata(root.join(&entry.path)) {
            Ok(_) => {
                if reader.inspect(&entry.path)? != entry.node {
                    return Err(Error::Invalid(format!(
                        "transferred auxiliary differs: {}",
                        entry.path
                    )));
                }
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let bytes = fs::read(
            root.join(CONTROL)
                .join("auxiliary")
                .join(&entry.node.content),
        )?;
        if bytes.len() as u64 != entry.node.len
            || alt_hash::blake3::hash(&bytes).to_hex().as_str() != entry.node.content
        {
            return Err(Error::Invalid("auxiliary transfer blob mismatch".into()));
        }
        match entry.node.kind {
            EntryKind::File => {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(root.join(&entry.path))?;
                file.write_all(&bytes)?;
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(if entry.node.executable {
                    0o755
                } else {
                    0o644
                }))?;
                file.sync_all()?;
            }
            EntryKind::Symlink => {
                use std::os::unix::ffi::OsStringExt;
                std::os::unix::fs::symlink(
                    std::ffi::OsString::from_vec(bytes),
                    root.join(&entry.path),
                )?;
            }
            EntryKind::Directory => unreachable!(),
        }
    }
    Ok(())
}
pub fn output_roots(manifest: &Manifest) -> Result<Vec<String>> {
    let outputs: Vec<String> = match manifest.context.validation.get("outputs") {
        Some(value) => alt_json::from_value(value.clone())?,
        None => Vec::new(),
    };
    for entry in &manifest.entries {
        if covers(CONTROL, &entry.path) {
            return Err(Error::Invalid(
                "input collides with observer control directory".into(),
            ));
        }
    }
    for output in &outputs {
        alt_input::checked_path(output.as_bytes())?;
        if covers(".alt", output) || covers(".git", output) || covers(CONTROL, output) {
            return Err(Error::Invalid(
                "output overlaps VCS/observer metadata".into(),
            ));
        }
        for input in manifest
            .entries
            .iter()
            .map(|entry| &entry.path)
            .chain(manifest.plan.auxiliary.iter().map(|root| &root.path))
            .chain(manifest.plan.exclusions.iter())
        {
            if covers(output, input) || covers(input, output) {
                return Err(Error::Invalid(format!("output overlaps input: {output}")));
            }
        }
        if outputs
            .iter()
            .any(|other| other != output && (covers(other, output) || covers(output, other)))
        {
            return Err(Error::Invalid("overlapping output roots".into()));
        }
    }
    Ok(outputs)
}

pub(crate) fn prepare_outputs(root: &Path, outputs: &[String]) -> Result<()> {
    for output in outputs {
        let mut path = root.to_path_buf();
        for part in output.split('/') {
            path.push(part);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err(Error::Invalid(
                        "output directory traverses a non-directory".into(),
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&path)?
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

/// Create into an empty directory, verify each blob, and create links last.
/// Parents are created before files and cannot be replaced by manifest links.
pub fn materialize(
    root: &Path,
    manifest: &Manifest,
    mut blob: impl FnMut(&str, u64) -> Result<Vec<u8>>,
) -> Result<()> {
    manifest.validate()?;
    output_roots(manifest)?;
    if fs::read_dir(root)?.next().is_some() {
        return Err(Error::Invalid(
            "materialization destination is not empty".into(),
        ));
    }
    for entry in &manifest.entries {
        let destination = root.join(&entry.path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        match entry.node.kind {
            EntryKind::Directory => fs::create_dir_all(&destination)?,
            EntryKind::Symlink => {}
            EntryKind::File => {
                let bytes = blob(&entry.node.content, entry.node.len)?;
                if bytes.len() as u64 != entry.node.len
                    || alt_hash::blake3::hash(&bytes).to_hex().as_str() != entry.node.content
                {
                    return Err(Error::Invalid(format!(
                        "blob identity mismatch: {}",
                        entry.path
                    )));
                }
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)?;
                file.write_all(&bytes)?;
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(if entry.node.executable {
                    0o755
                } else {
                    0o644
                }))?;
                file.sync_all()?;
            }
        }
    }
    for entry in &manifest.entries {
        if entry.node.kind == EntryKind::Symlink {
            use std::os::unix::ffi::OsStringExt;
            let target = alt_input::unhex(
                entry
                    .node
                    .link_target
                    .as_deref()
                    .ok_or_else(|| Error::Invalid("missing link target".into()))?,
            )?;
            std::os::unix::fs::symlink(
                std::ffi::OsString::from_vec(target),
                root.join(&entry.path),
            )?;
        }
    }
    let observed = observe_inputs(root, manifest, &output_roots(manifest)?);
    if observed.identity.as_deref() != Some(&manifest.id()?) {
        return Err(Error::Invalid(format!(
            "materialized inputs differ: {:?}",
            observed.errors
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct InputObservation {
    pub identity: Option<String>,
    pub manifest: Option<Manifest>,
    pub unexpected: Vec<String>,
    pub errors: Vec<String>,
}

/// Re-scan source bytes from disk as well as directory membership. The index
/// is deliberately absent here: a formatter can change disk without staging.
pub fn observe_inputs(root: &Path, expected: &Manifest, outputs: &[String]) -> InputObservation {
    let mut observation = InputObservation {
        identity: None,
        manifest: None,
        unexpected: Vec::new(),
        errors: Vec::new(),
    };
    let result = (|| -> Result<()> {
        let reader = Root::open(root)?;
        let actual = alt_input::scan(
            &reader,
            &reader,
            expected.plan.clone(),
            expected.context.clone(),
        )?;
        let mut known = BTreeSet::new();
        for entry in &actual.entries {
            known.insert(entry.path.clone());
            for (at, _) in entry.path.match_indices('/') {
                known.insert(entry.path[..at].to_owned());
            }
        }
        for output in outputs {
            for (at, _) in output.match_indices('/') {
                known.insert(output[..at].to_owned());
            }
        }
        fn visit(
            reader: &Root,
            path: &str,
            expected: &Manifest,
            outputs: &[String],
            known: &BTreeSet<String>,
            unexpected: &mut Vec<String>,
        ) -> Result<()> {
            if path == CONTROL || outputs.iter().any(|output| covers(output, path)) {
                return Ok(());
            }
            if expected
                .plan
                .exclusions
                .iter()
                .any(|excluded| covers(excluded, path))
            {
                // Excluded inputs are not secretly copied or hashed, but a
                // materialized run must not acquire unknown secret paths.
                unexpected.push(path.into());
                return Ok(());
            }
            if !known.contains(path) {
                unexpected.push(path.into());
            }
            if reader.inspect(path)?.kind == EntryKind::Directory {
                for child in reader.children(path)? {
                    visit(reader, &child, expected, outputs, known, unexpected)?;
                }
            }
            Ok(())
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            let path = alt_input::checked_path(name.as_encoded_bytes())?;
            visit(
                &reader,
                path,
                expected,
                outputs,
                &known,
                &mut observation.unexpected,
            )?;
        }
        observation.unexpected.sort();
        observation.unexpected.dedup();
        if observation.unexpected.is_empty() {
            observation.identity = Some(actual.id()?);
        }
        observation.manifest = Some(actual);
        Ok(())
    })();
    if let Err(error) = result {
        observation.errors.push(error.to_string());
    }
    observation
}

pub(crate) fn capture_outputs(
    root: &Path,
    control: &Path,
    outputs: &[String],
    captures: &mut BTreeMap<String, String>,
    artifacts: &mut BTreeMap<String, Artifact>,
) -> Result<BTreeMap<String, Node>> {
    let reader = Root::open(root)?;
    let mut inventory = BTreeMap::new();
    fn visit(
        root: &Path,
        reader: &Root,
        control: &Path,
        path: &str,
        captures: &mut BTreeMap<String, String>,
        artifacts: &mut BTreeMap<String, Artifact>,
        inventory: &mut BTreeMap<String, Node>,
    ) -> Result<()> {
        let node = reader.inspect(path)?;
        if node.kind == EntryKind::Directory {
            for child in reader.children(path)? {
                visit(
                    root, reader, control, &child, captures, artifacts, inventory,
                )?;
            }
        } else if node.kind == EntryKind::File && node.executable {
            let artifact = capture_file(control, &root.join(path), captures)?;
            if artifact.blake3 != node.content || artifact.bytes != node.len {
                return Err(Error::Invalid("output changed during capture".into()));
            }
            artifacts.insert(format!("executable:{path}"), artifact);
        }
        inventory.insert(path.into(), node);
        Ok(())
    }
    for output in outputs {
        match fs::symlink_metadata(root.join(output)) {
            Ok(_) => visit(
                root,
                &reader,
                control,
                output,
                captures,
                artifacts,
                &mut inventory,
            )?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(inventory)
}
