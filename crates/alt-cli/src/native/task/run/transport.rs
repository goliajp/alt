use super::*;
use alt_task::RemoteRun;
use std::process::ExitStatus;

struct Capture {
    status: ExitStatus,
    stdout: PathBuf,
    stderr: PathBuf,
}
fn logs(root: &Path) -> Res<PathBuf> {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid run directory")?;
    let directory = root
        .parent()
        .ok_or("run has no parent")?
        .join(format!(".{name}-transport"));
    fs::create_dir_all(&directory)?;
    Ok(directory)
}
fn invoke(root: &Path, label: &str, command: &mut Command) -> Res<Capture> {
    invoke_input(root, label, command, None)
}
fn invoke_input(
    root: &Path,
    label: &str,
    command: &mut Command,
    input: Option<&[u8]>,
) -> Res<Capture> {
    let directory = logs(root)?;
    let stdout = alt_os::temp::NamedTempFile::new_in(&directory)?;
    let stderr = alt_os::temp::NamedTempFile::new_in(&directory)?;
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(stdout.as_file().try_clone()?)
        .stderr(stderr.as_file().try_clone()?)
        .spawn()?;
    // Always reap the child and retain its streams, including a broken input
    // pipe when the observer or transport exits before accepting the key.
    let input_error = input.and_then(|input| {
        child.stdin.take().map_or_else(
            || Some(std::io::Error::other("missing observer input pipe")),
            |mut pipe| pipe.write_all(input).err(),
        )
    });
    let status = child.wait()?;
    let (_, stdout) = stdout.keep()?;
    let (_, stderr) = stderr.keep()?;
    let mut log = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(directory.join("operations.jsonl"))?;
    serde_json::to_writer(
        &mut log,
        &json!({"step":label,"stdout":stdout,"stderr":stderr,"exit_code":status.code(),"success":status.success(),"input_error":input_error.as_ref().map(ToString::to_string)}),
    )?;
    writeln!(log)?;
    log.sync_all()?;
    if let Some(error) = input_error {
        return Err(error.into());
    }
    Ok(Capture {
        status,
        stdout,
        stderr,
    })
}
fn ensure_success(result: &Capture, message: &str) -> Res<()> {
    if !result.status.success() {
        let mut failure = crate::machine::Failure::unknown(message);
        *failure.details = json!({"exit_code":result.status.code(),"stdout_path":result.stdout,"stderr_path":result.stderr});
        return Err(failure.into());
    }
    Ok(())
}
fn quote(value: &str) -> Res<String> {
    if value.contains(['\0', '\n', '\r']) {
        return Err(refused("unsupported remote argument"));
    }
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}
fn ssh(destination: &str, command: &str) -> Res<Command> {
    if destination.starts_with('-')
        || !destination.contains('@')
        || !destination
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@._:-[]".contains(&b))
    {
        return Err(refused("invalid resolved SSH destination"));
    }
    let mut process = Command::new("ssh");
    process.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=2",
        "--",
        destination,
        command,
    ]);
    Ok(process)
}
fn remote_file(root: &Path, remote: &RemoteRun, path: &str, label: &str) -> Res<PathBuf> {
    let result = invoke(
        root,
        label,
        &mut ssh(&remote.destination, &format!("cat -- {}", quote(path)?))?,
    )?;
    ensure_success(
        &result,
        "remote file is missing or transport is unavailable; preserve the attempt and collect later",
    )?;
    Ok(result.stdout)
}
fn same_artifact(store: &mut alt_task::Store, path: &Path, expected: &Artifact) -> Res<()> {
    let actual = captured(store, path)?;
    if &actual != expected {
        return Err(refused(
            "observed artifact does not match its expected digest/length",
        ));
    }
    Ok(())
}

pub(super) fn prepare(
    root: &Path,
    profile: &Profile,
    spec: &alt_runner::Spec,
    store: &mut alt_task::Store,
) -> Res<(Option<RemoteRun>, String, Artifact)> {
    match profile {
        Profile::Local { .. } => {
            let observer = std::env::current_exe()?;
            let artifact = captured(store, &observer)?;
            // Match the remote snapshot, where sync excludes native metadata.
            // Keeping it under observer control preserves the source snapshot.
            fs::rename(
                root.join(".alt"),
                root.join(alt_runner::CONTROL).join("native-snapshot"),
            )?;
            Ok((
                None,
                observer.to_str().ok_or("unsupported observer path")?.into(),
                artifact,
            ))
        }
        Profile::RemoteSync {
            device,
            sync_program,
            observer,
            observer_blake3,
            max_wait_seconds,
            ..
        } => {
            alt_task::check_id(observer_blake3)?;
            if !sync_program.is_absolute()
                || !Path::new(observer).is_absolute()
                || *max_wait_seconds == 0
                || *max_wait_seconds > 3600
            {
                return Err(refused(
                    "remote profile requires absolute executables and a bounded lock wait",
                ));
            }
            let program = captured(store, sync_program)?;
            let name = format!("alt-task-{}", spec.nonce);
            let mut command = Command::new(sync_program);
            command.current_dir(root).args([
                device,
                "--name",
                &name,
                "--also",
                alt_runner::CONTROL,
            ]);
            let mut auxiliary = std::collections::BTreeSet::new();
            for input in &spec.manifest.plan.auxiliary {
                let Some(entry) = spec
                    .manifest
                    .entries
                    .iter()
                    .find(|entry| entry.path == input.path)
                else {
                    continue;
                };
                if entry.node.kind == alt_input::EntryKind::Directory {
                    auxiliary.insert(input.path.clone());
                } else if let Some((parent, _)) = input.path.rsplit_once('/') {
                    auxiliary.insert(parent.into());
                }
            }
            for path in auxiliary {
                command.args(["--also", &path]);
            }
            let result = invoke(root, "canonical-remote-sync", &mut command)?;
            let stdout = captured(store, &result.stdout)?;
            let stderr = captured(store, &result.stderr)?;
            ensure_success(
                &result,
                "canonical remote-sync did not complete; no validation was launched",
            )?;
            same_artifact(store, sync_program, &program)?;
            let output = fs::read_to_string(&result.stdout)?;
            let destination = output
                .lines()
                .filter_map(|line| line.rsplit_once(" -> ").map(|(_, tail)| tail))
                .filter_map(|tail| tail.split_once(":work/"))
                .find(|(_, path)| path.starts_with(&format!("{name}/ (")))
                .map(|(destination, _)| destination.to_owned())
                .ok_or("sync result did not identify its resolved destination")?;
            let result = invoke(
                root,
                "resolve-remote-directory",
                &mut ssh(
                    &destination,
                    &format!("cd {} && pwd -P", quote(&format!("work/{name}"))?),
                )?,
            )?;
            ensure_success(&result, "cannot resolve materialized remote directory")?;
            let directory = fs::read_to_string(&result.stdout)?.trim().to_owned();
            if !Path::new(&directory).is_absolute()
                || directory.contains(['\n', '\r', '\0'])
                || Path::new(&directory).file_name().and_then(|v| v.to_str()) != Some(&name)
            {
                return Err(refused("unexpected remote materialization directory"));
            }
            let remote = RemoteRun {
                destination,
                directory,
                max_wait_seconds: *max_wait_seconds,
                sync_program: program,
                sync_stdout: stdout,
                sync_stderr: stderr,
            };
            let binary = remote_file(root, &remote, observer, "observer-before")?;
            let artifact = captured(store, &binary)?;
            if &artifact.blake3 != observer_blake3 {
                return Err(refused(
                    "remote observer binary is not the pinned binary; no validation was launched",
                ));
            }
            Ok((Some(remote), observer.clone(), artifact))
        }
    }
}

pub(super) fn launch(attempt: &RunAttempt, authentication_key: &[u8; 32]) -> Res<()> {
    let root = Path::new(&attempt.local_directory);
    let result = if let Some(remote) = &attempt.remote {
        let command = format!(
            "cd {} && echo $$ > .alt-runner/launcher.pid && exec nohup /usr/local/bin/bench-lock heavy --max-wait {} {} runner execute .alt-runner/spec.json --json > .alt-runner/observer.stdout 2> .alt-runner/observer.stderr",
            quote(&remote.directory)?,
            remote.max_wait_seconds,
            quote(&attempt.observer_path)?
        );
        invoke_input(
            root,
            "launch-observer",
            &mut ssh(&remote.destination, &command)?,
            Some(authentication_key),
        )?
    } else {
        let mut command = Command::new(&attempt.observer_path);
        command
            .current_dir(root)
            .args(["runner", "execute", ".alt-runner/spec.json", "--json"]);
        invoke_input(
            root,
            "launch-observer",
            &mut command,
            Some(authentication_key),
        )?
    };
    ensure_success(
        &result,
        "observer/transport did not acknowledge a complete report",
    )
}

pub(super) fn collect(
    attempt: &RunAttempt,
    store: &mut alt_task::Store,
) -> Res<(Evidence, Artifact)> {
    let root = Path::new(&attempt.local_directory);
    let original_spec = store.blob(&attempt.spec)?;
    let spec: alt_runner::Spec = serde_json::from_slice(&original_spec)?;
    if spec.nonce != attempt.nonce
        || spec.candidate != attempt.candidate
        || spec.manifest.id()? != attempt.manifest
    {
        return Err(refused("stored attempt/spec identity mismatch"));
    }
    let read = |relative: &str| -> Res<PathBuf> {
        alt_input::checked_path(relative.as_bytes())?;
        if let Some(remote) = &attempt.remote {
            remote_file(
                root,
                remote,
                &format!("{}/{}/{relative}", remote.directory, alt_runner::CONTROL),
                "collect-observation",
            )
        } else {
            Ok(root.join(alt_runner::CONTROL).join(relative))
        }
    };
    let report_path = read("report.json")?;
    let bytes = fs::read(&report_path)?;
    let key = store.blob(&attempt.authentication_key)?;
    let key: [u8; 32] = key
        .try_into()
        .map_err(|_| refused("invalid observer authentication key"))?;
    let mac = fs::read_to_string(read("report.mac")?)?;
    if alt_hash::blake3::keyed_hash(&key, &bytes).to_hex().as_str() != mac.trim() {
        return Err(refused("observer report authentication failed"));
    }
    let report: alt_runner::Report = serde_json::from_slice(&bytes)?;
    if report.schema_version != 1
        || report.nonce != attempt.nonce
        || report.evidence.candidate != attempt.candidate
        || report.evidence.manifest != attempt.manifest
        || report.evidence.provenance != alt_task::Provenance::Observed
    {
        return Err(refused(
            "observer report does not match the launched attempt",
        ));
    }
    let observer_path = if let Some(remote) = &attempt.remote {
        remote_file(root, remote, &attempt.observer_path, "observer-after")?
    } else {
        PathBuf::from(&attempt.observer_path)
    };
    same_artifact(store, &observer_path, &attempt.observer)?;
    let mut evidence = report.evidence;
    for artifact in evidence
        .binaries
        .values()
        .chain(evidence.artifacts.values())
        .chain(evidence.stdout.iter())
        .chain(evidence.stderr.iter())
    {
        let expected = format!("captures/{}", artifact.blake3);
        if report.captures.get(&artifact.blake3) != Some(&expected) {
            return Err(refused("observer capture mapping is incomplete or unsafe"));
        }
        let file = read(&expected)?;
        same_artifact(store, &file, artifact)?;
    }
    if !report.problems.is_empty() {
        evidence.after = None;
    }
    evidence
        .artifacts
        .insert("observer_binary".into(), attempt.observer.clone());
    evidence
        .artifacts
        .insert("observer_spec".into(), attempt.spec.clone());
    if let Some(remote) = &attempt.remote {
        evidence
            .artifacts
            .insert("sync_program".into(), remote.sync_program.clone());
        evidence
            .artifacts
            .insert("sync_stdout".into(), remote.sync_stdout.clone());
        evidence
            .artifacts
            .insert("sync_stderr".into(), remote.sync_stderr.clone());
    }
    let report_artifact = store.put_blob(&bytes)?;
    Ok((evidence, report_artifact))
}
