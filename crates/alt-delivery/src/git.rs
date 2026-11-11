use super::*;
use alt_bytes::BString;
use alt_git_pack::PackWriter;
use std::{path::PathBuf, process::Command};

/// Only self-contained inert configuration crosses the task boundary. Values
/// are never echoed in refusal diagnostics (they may contain credentials).
pub fn validate_config(bytes: &[u8]) -> Result<()> {
    let config = alt_git_config::Config {
        entries: alt_git_config::parse_file(bytes)?,
    };
    for entry in &config.entries {
        let allowed = match entry.section.as_str() {
            "core" => matches!(
                entry.key.as_str(),
                "repositoryformatversion"
                    | "filemode"
                    | "bare"
                    | "logallrefupdates"
                    | "ignorecase"
                    | "precomposeunicode"
                    | "symlinks"
                    | "quotepath"
                    | "autocrlf"
                    | "safecrlf"
                    | "eol"
            ),
            "extensions" => matches!(entry.key.as_str(), "objectformat" | "refstorage"),
            "user" => matches!(entry.key.as_str(), "name" | "email"),
            "remote" => matches!(entry.key.as_str(), "url" | "pushurl" | "fetch" | "push"),
            "branch" => matches!(
                entry.key.as_str(),
                "remote" | "merge" | "rebase" | "pushremote"
            ),
            "init" => entry.key == "defaultbranch",
            "gitflow" => true,
            _ => false,
        };
        if !allowed {
            return Err(format!(
                "unsupported task Git config key: {}.{}",
                entry.section, entry.key
            )
            .into());
        }
        if entry.section == "remote" && matches!(entry.key.as_str(), "url" | "pushurl") {
            let bytes = entry
                .value
                .as_ref()
                .ok_or("remote URL requires a value")?
                .as_slice();
            let value = std::str::from_utf8(bytes)?;
            if value.contains(['\n', '\r', '?', '#'])
                || value.contains("::")
                || value.split_once("://").is_some_and(|(_, rest)| {
                    rest.split('/')
                        .next()
                        .is_some_and(|host| host.contains('@'))
                })
            {
                return Err(
                    "credential-bearing or external-helper remote URL is unsupported".into(),
                );
            }
        }
    }
    if config
        .get_bool("core", None, "bare")
        .transpose()?
        .unwrap_or(false)
    {
        return Err("bare task intake is unsupported".into());
    }
    Ok(())
}
pub fn destination(target: &Path) -> Result<PathBuf> {
    let name = target.file_name().ok_or("destination needs a name")?;
    alt_input::checked_path(name.as_encoded_bytes())?;
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent)?;
    alt_input::check_execution_platform(&parent)?;
    for ancestor in parent.ancestors() {
        if fs::symlink_metadata(ancestor.join(".git")).is_ok()
            || fs::symlink_metadata(ancestor.join(".alt")).is_ok()
        {
            return Err("destination must be outside every repository".into());
        }
    }
    let target = parent.join(name);
    if fs::symlink_metadata(&target).is_ok() {
        return Err("destination must not exist; choose a new independent directory".into());
    }
    Ok(target)
}
#[derive(Clone, Debug, Serialize, Deserialize, ToJson, FromJson)]
pub struct GitCheck {
    pub program: String,
    pub binary_blake3: String,
    pub version_stdout_hex: String,
    pub version_stderr_hex: String,
    pub fsck_stdout_hex: String,
    pub fsck_stderr_hex: String,
    pub fsck_exit: i32,
}
fn git_command(git: &Path, root: &Path, args: &[&str]) -> Result<std::process::Output> {
    if !git.is_absolute() {
        return Err("Git verifier must be an absolute executable path".into());
    }
    Ok(Command::new(git)
        .current_dir(root)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .args(args)
        .output()?)
}
pub fn git_check(git: &Path, root: &Path) -> Result<GitCheck> {
    let before = alt_hash::blake3::hash(&fs::read(git)?).to_hex().to_string();
    let version = git_command(git, root, &["--version"])?;
    if !version.status.success() {
        return Err(format!(
            "Git version probe failed: exit={:?} stdout_hex={} stderr_hex={}",
            version.status.code(),
            alt_input::hex(&version.stdout),
            alt_input::hex(&version.stderr)
        )
        .into());
    }
    let fsck = git_command(git, root, &["fsck", "--strict", "--full"])?;
    if alt_hash::blake3::hash(&fs::read(git)?).to_hex().as_str() != before {
        return Err("Git verifier changed during execution".into());
    }
    let report = GitCheck {
        program: git.to_str().ok_or("non-UTF-8 Git verifier path")?.into(),
        binary_blake3: before,
        version_stdout_hex: alt_input::hex(&version.stdout),
        version_stderr_hex: alt_input::hex(&version.stderr),
        fsck_stdout_hex: alt_input::hex(&fsck.stdout),
        fsck_stderr_hex: alt_input::hex(&fsck.stderr),
        fsck_exit: fsck.status.code().unwrap_or(-1),
    };
    if !fsck.status.success() {
        return Err(format!("Git verification failed: {}", alt_json::to_string(&report)?).into());
    }
    Ok(report)
}
fn fingerprint(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                return Err("Git metadata links/special files are unsupported".into());
            }
            if kind.is_dir() {
                queue.push(path);
            } else {
                result.insert(
                    alt_input::hex(path.strip_prefix(root)?.as_os_str().as_encoded_bytes()),
                    alt_hash::blake3::hash(&fs::read(path)?)
                        .to_hex()
                        .to_string(),
                );
            }
        }
    }
    Ok(result)
}
fn source_preflight(source: &Path) -> Result<()> {
    alt_input::check_execution_platform(source)?;
    let dir = source.join(".git");
    if !fs::symlink_metadata(&dir)?.file_type().is_dir()
        || fs::symlink_metadata(source.join(".alt")).is_ok()
    {
        return Err("intake requires an ordinary Git worktree without .alt".into());
    }
    for unsupported in [
        "commondir",
        "gitdir",
        "shallow",
        "objects/info/alternates",
        "objects/info/http-alternates",
        "info/sparse-checkout",
        "config.worktree",
        "info/grafts",
    ] {
        if dir.join(unsupported).exists() {
            return Err(format!("unsupported Git form: {unsupported}").into());
        }
    }
    fn structural(path: &Path) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                return Err("Git metadata links/special files are unsupported".into());
            }
            if kind.is_dir() {
                structural(&entry.path())?;
            }
        }
        Ok(())
    }
    structural(&dir)?;
    validate_config(&fs::read(dir.join("config"))?)?;
    for relative in ["info/attributes", "info/exclude"] {
        if let Ok(bytes) = fs::read(dir.join(relative))
            && bytes
                .split(|b| *b == b'\n')
                .map(|line| line.trim_ascii())
                .any(|line| !line.is_empty() && !line.starts_with(b"#"))
        {
            return Err(format!(
                "repository-local {relative} rules require explicit standalone input"
            )
            .into());
        }
    }

    if let Ok(entries) = fs::read_dir(dir.join("objects/pack")) {
        for entry in entries {
            if entry?.path().extension().is_some_and(|e| e == "promisor") {
                return Err("promisor objects are unsupported".into());
            }
        }
    }
    if let Ok(entries) = fs::read_dir(dir.join("hooks")) {
        for entry in entries {
            let entry = entry?;
            if !entry.file_name().as_encoded_bytes().ends_with(b".sample") {
                return Err("source hooks require explicit external-input authorization".into());
            }
        }
    }
    // Detect links before Repository::discover could follow external metadata.
    fingerprint(&dir)?;
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize, ToJson, FromJson)]
pub struct IntakeReport {
    pub baseline: Snapshot,
    pub source_metadata_blake3: String,
    pub git: GitCheck,
}
pub fn intake(source: &Path, target: &Path, git: &Path) -> Result<IntakeReport> {
    let source = fs::canonicalize(source)?;
    source_preflight(&source)?;
    let target = destination(target)?;
    let before = fingerprint(&source.join(".git"))?;
    let git_report = git_check(git, &source)?;
    let repo = alt_repo::Repository::discover(&source)?;
    let staging = alt_os::temp::tempdir_in(target.parent().ok_or("target parent missing")?)?;
    let alt_dir = staging.path().join(".alt");
    alt_import::import_git(&repo, &alt_dir, "task-intake", 0)?;
    let odb = NativeOdb::open(&alt_dir)?;
    let refs = alt_refs::RefStore::open(&alt_dir)?;
    let selected = refs
        .iter()
        .map(|(n, t)| (n.to_owned(), render(t)))
        .collect();
    let baseline = snapshot(&odb, selected, &config_bytes(&alt_dir)?, repo.algo())?;
    // Every ref graph must be complete, even if it is not the checkout HEAD.
    closure(
        &odb,
        baseline
            .refs
            .keys()
            .map(|n| resolve(&baseline.refs, n))
            .collect::<Result<Vec<_>>>()?,
    )?;
    let entries = files(&odb, resolve(&baseline.refs, "HEAD")?)?;
    preflight_paths(staging.path(), &entries, &odb)?;
    materialize(staging.path(), &entries, &odb)?;
    if before != fingerprint(&source.join(".git"))? {
        return Err("source Git metadata changed during intake".into());
    }
    let report = IntakeReport {
        baseline,
        source_metadata_blake3: alt_input::identity_json("source-git-metadata", &before)?,
        git: git_report,
    };
    fs::write(
        alt_dir.join("delivery-origin.json"),
        alt_json::to_vec(&report.baseline)?,
    )?;
    fs::write(
        alt_dir.join("intake-report.json"),
        alt_json::to_vec(&report)?,
    )?;
    // Attach the native index in the staging directory before publication.
    // Intake's canonical checkout and index construction stay in Rust.
    let entries: Vec<_> = entries
        .iter()
        .map(|(path, e)| alt_git_index_entry(path, e))
        .collect::<Result<_>>()?;
    write_index(&alt_dir, &entries, repo.algo())?;
    drop(refs);
    drop(odb);
    publish_directory(staging.path(), &target)?;
    Ok(report)
}

#[derive(Clone, Debug, Serialize, Deserialize, ToJson, FromJson)]
pub struct ExportReport {
    pub baseline_id: String,
    pub exported: Snapshot,
    pub files: BTreeMap<String, FileEntry>,
    pub git: GitCheck,
    pub config_transform: String,
}
pub fn prepare_export(
    odb: &NativeOdb,
    baseline: &Snapshot,
    refs: BTreeMap<String, String>,
    extra_roots: &[ObjectId],
    target: &Path,
    git: &Path,
) -> Result<PreparedExport> {
    baseline.validate(odb)?;
    let target = destination(target)?;
    let mut exported = baseline.clone();
    exported.refs = refs;
    let roots = exported
        .refs
        .keys()
        .map(|name| resolve(&exported.refs, name))
        .chain(extra_roots.iter().copied().map(Ok))
        .collect::<Result<Vec<_>>>()?;
    exported.objects.extend(closure(odb, roots)?);
    let config = portable_config(&alt_input::unhex(&baseline.config_hex)?)?;
    exported.config_hex = alt_input::hex(&config);
    exported.validate(odb)?;
    let entries = files(odb, resolve(&exported.refs, "HEAD")?)?;
    let staging = alt_os::temp::tempdir_in(target.parent().ok_or("missing destination parent")?)?;
    preflight_paths(staging.path(), &entries, odb)?;
    super::paths::probe_names(staging.path(), exported.refs.keys().map(String::as_str))?;
    let dir = staging.path().join(".git");
    fs::create_dir_all(dir.join("refs"))?;
    fs::write(dir.join("config"), &config)?;
    let mut pack = PackWriter::create(
        &dir.join("objects/pack"),
        exported.algo()?,
        u32::try_from(exported.objects.len())?,
    )?;
    for id in exported.objects.keys() {
        let oid = ObjectId::from_hex(id.as_bytes())?;
        let object = odb.get(&oid)?.ok_or("export object missing")?;
        pack.add(oid, object.kind, &object.data)?;
    }
    pack.finish()?;
    let mut head = None;
    let mut loose = Vec::new();
    for (name, value) in &exported.refs {
        let value = if let Some(sym) = value.strip_prefix("ref: ") {
            alt_git_refs::RefTarget::Symbolic(BString::from(sym))
        } else {
            alt_git_refs::RefTarget::Direct(ObjectId::from_hex(value.as_bytes())?)
        };
        if name == "HEAD" {
            head = Some(value);
        } else {
            loose.push((BString::from(name.as_str()), value));
        }
    }
    alt_git_refs::write_loose(&dir, head.as_ref(), &loose)?;
    materialize(staging.path(), &entries, odb)?;
    let status = git_command(git, staging.path(), &["read-tree", "HEAD"])?;
    if !status.status.success() {
        return Err(format!(
            "Git index creation failed: exit={:?} stdout_hex={} stderr_hex={}",
            status.status.code(),
            alt_input::hex(&status.stdout),
            alt_input::hex(&status.stderr)
        )
        .into());
    }
    let git_report = verify_repository(odb, &exported, &entries, staging.path(), git)?;
    let report = ExportReport {
        baseline_id: baseline.id()?,
        exported,
        files: entries,
        git: git_report,
        config_transform: "files refs; extensions.refstorage removed".into(),
    };
    Ok(PreparedExport {
        staging,
        target,
        report,
    })
}

fn alt_git_index_entry(path: &str, e: &FileEntry) -> Result<alt_git_index::IndexEntry> {
    Ok(alt_git_index::IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode: e.mode,
        uid: 0,
        gid: 0,
        size: 0,
        oid: ObjectId::from_hex(e.oid.as_bytes())?,
        flags: 0,
        extended_flags: None,
        path: BString::from(path),
    })
}
fn write_index(
    alt_dir: &Path,
    entries: &[alt_git_index::IndexEntry],
    algo: HashAlgo,
) -> Result<()> {
    let index = alt_git_index::Index {
        version: 2,
        entries: entries.to_vec(),
        extensions: Vec::new(),
    };
    fs::write(alt_dir.join("index"), index.serialize(algo))?;
    Ok(())
}

/// Preserve parsed Git semantics, including unusual valid section formatting,
/// while converting only the physical ref backend declaration to files.
fn portable_config(bytes: &[u8]) -> Result<Vec<u8>> {
    fn quoted(out: &mut Vec<u8>, bytes: &[u8]) {
        out.push(b'"');
        for byte in bytes {
            match byte {
                b'\\' => out.extend_from_slice(b"\\\\"),
                b'"' => out.extend_from_slice(b"\\\""),
                b'\n' => out.extend_from_slice(b"\\n"),
                b'\t' => out.extend_from_slice(b"\\t"),
                8 => out.extend_from_slice(b"\\b"),
                byte => out.push(*byte),
            }
        }
        out.push(b'"');
    }
    let mut out = Vec::new();
    let entries = alt_git_config::parse_file(bytes)?;
    for entry in &entries {
        if entry.section == "extensions" && entry.key == "refstorage" {
            continue;
        }
        out.push(b'[');
        out.extend_from_slice(entry.section.as_bytes());
        if let Some(sub) = &entry.subsection {
            out.push(b' ');
            quoted(&mut out, sub.as_slice());
        }
        out.extend_from_slice(b"]\n\t");
        out.extend_from_slice(entry.key.as_bytes());
        if let Some(value) = &entry.value {
            out.extend_from_slice(b" = ");
            quoted(&mut out, value.as_slice());
        }
        out.push(b'\n');
    }
    let expected: Vec<_> = entries
        .into_iter()
        .filter(|e| e.section != "extensions" || e.key != "refstorage")
        .collect();
    if alt_git_config::parse_file(&out)? != expected {
        return Err("portable configuration changed semantics".into());
    }
    Ok(out)
}

pub struct PreparedExport {
    staging: alt_os::temp::TempDir,
    target: PathBuf,
    pub report: ExportReport,
}
impl PreparedExport {
    pub fn publish(self) -> Result<ExportReport> {
        publish_directory(self.staging.path(), &self.target)?;
        Ok(self.report)
    }
}
pub fn export(
    odb: &NativeOdb,
    baseline: &Snapshot,
    refs: BTreeMap<String, String>,
    extra_roots: &[ObjectId],
    target: &Path,
    git: &Path,
) -> Result<ExportReport> {
    prepare_export(odb, baseline, refs, extra_roots, target, git)?.publish()
}
fn verify_repository(
    odb: &NativeOdb,
    expected: &Snapshot,
    entries: &BTreeMap<String, FileEntry>,
    root: &Path,
    git: &Path,
) -> Result<GitCheck> {
    source_preflight(root)?;
    let config = fs::read(root.join(".git/config"))?;
    if alt_git_config::parse_file(&config)?
        != alt_git_config::parse_file(&alt_input::unhex(&expected.config_hex)?)?
    {
        return Err("delivery config semantics differ".into());
    }
    let observed = alt_repo::Repository::discover(root)?;
    let git_refs = observed.git_refs().ok_or("delivery Git refs missing")?;
    let render = |target: alt_git_refs::RefTarget| -> Result<String> {
        Ok(match target {
            alt_git_refs::RefTarget::Direct(oid) => oid.to_string(),
            alt_git_refs::RefTarget::Symbolic(name) => {
                format!("ref: {}", std::str::from_utf8(name.as_slice())?)
            }
        })
    };
    let mut actual = BTreeMap::new();
    actual.insert(
        "HEAD".into(),
        render(git_refs.read("HEAD")?.ok_or("delivery HEAD missing")?)?,
    );
    for reference in git_refs.iter_refs()? {
        actual.insert(
            std::str::from_utf8(reference.name.as_slice())?.into(),
            render(reference.target)?,
        );
    }
    if actual != expected.refs {
        return Err("delivery refs differ from the selected set".into());
    }
    let all = git_command(
        git,
        root,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname)",
        ],
    )?;
    if !all.status.success() {
        return Err(format!(
            "object inventory failed: stdout_hex={} stderr_hex={}",
            alt_input::hex(&all.stdout),
            alt_input::hex(&all.stderr)
        )
        .into());
    }
    let ids: BTreeSet<_> = std::str::from_utf8(&all.stdout)?
        .lines()
        .map(str::to_owned)
        .collect();
    if ids != expected.objects.keys().cloned().collect() {
        return Err("delivery object set differs from the selected inventory".into());
    }
    for (id, identity) in &expected.objects {
        let oid = ObjectId::from_hex(id.as_bytes())?;
        let object = observed
            .read_object(&oid)?
            .ok_or("exported object missing")?;
        if ObjectId::hash_object(oid.algo(), object.kind, &object.data) != oid
            || alt_hash::blake3::hash(&object.data).to_hex().as_str() != identity.blake3
        {
            return Err("exported canonical object differs".into());
        }
    }
    verify_checkout(root, entries, odb)?;
    git_check(git, root)
}
/// Verify a previously published directory without creating or changing files.
pub fn verify_existing(
    odb: &NativeOdb,
    report: &ExportReport,
    root: &Path,
    git: &Path,
) -> Result<GitCheck> {
    if !fs::symlink_metadata(root)?.file_type().is_dir() {
        return Err("delivery root is not an ordinary directory".into());
    }
    report.exported.validate(odb)?;
    verify_repository(odb, &report.exported, &report.files, root, git)
}

#[cfg(test)]
mod config_tests {
    use super::*;
    #[test]
    fn files_backend_conversion_preserves_independent_git_config_semantics() {
        let original=b"[core]\n bare = false\n fileMode\n[extensions] # retained semantics, physical backend changes\n refStorage = reftable\n[remote \"Origin\"]\n url = https://example.invalid/repo.git\n fetch = +refs/heads/*:refs/remotes/Origin/*\n[user]\n name = \"quoted\\\" tab\\t slash\\\\ line\\nend\"\n";
        let portable = portable_config(original).unwrap();
        let temp = alt_os::temp::tempdir().unwrap();
        let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|p| p.join("git"))
            .find(|p| p.is_file())
            .unwrap();
        let mut parsed = Vec::new();
        for (name, bytes) in [
            ("original", original.as_slice()),
            ("portable", portable.as_slice()),
        ] {
            let path = temp.path().join(name);
            fs::write(&path, bytes).unwrap();
            let output = Command::new(&git)
                .args(["config", "--null", "--file"])
                .arg(path)
                .arg("--list")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            parsed.push(
                output
                    .stdout
                    .split(|b| *b == 0)
                    .filter(|entry| !entry.starts_with(b"extensions.refstorage\n"))
                    .map(<[u8]>::to_vec)
                    .collect::<Vec<_>>(),
            );
        }
        assert_eq!(parsed[0], parsed[1]);
    }
}
