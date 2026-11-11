//! Canonical, independently checked Git boundaries. No private task records
//! or workspace refs are implicitly selected for delivery.
use alt_git_codec::{Commit, HashAlgo, ObjectId, ObjectKind, Tag, Tree};
use alt_json::{FromJson, ToJson};
use alt_odb::NativeOdb;
use alt_refs::RefTarget;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
mod git;
mod paths;
pub use git::{
    ExportReport, GitCheck, IntakeReport, PreparedExport, destination, export, git_check, intake,
    prepare_export, validate_config, verify_existing,
};
pub use paths::{materialize, preflight_paths, publish_directory, validate_files, verify_checkout};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToJson, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: u32,
    pub object_format: String,
    /// Every original canonical object, including unreachable source objects.
    pub objects: BTreeMap<String, ObjectIdentity>,
    /// HEAD plus explicitly selected public refs; symbolic values use `ref: `.
    pub refs: BTreeMap<String, String>,
    pub config_hex: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToJson, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct ObjectIdentity {
    pub kind: String,
    pub bytes: u64,
    pub blake3: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToJson, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct FileEntry {
    pub mode: u32,
    pub oid: String,
}
impl Snapshot {
    pub fn id(&self) -> Result<String> {
        Ok(alt_input::identity_json("delivery-baseline", self)?)
    }
    pub fn algo(&self) -> Result<HashAlgo> {
        match self.object_format.as_str() {
            "sha1" => Ok(HashAlgo::Sha1),
            "sha256" => Ok(HashAlgo::Sha256),
            _ => Err("unsupported object format".into()),
        }
    }
    pub fn validate(&self, odb: &NativeOdb) -> Result<()> {
        if self.schema_version != 1 {
            return Err("unsupported delivery baseline".into());
        }
        let algo = self.algo()?;
        validate_config(&alt_input::unhex(&self.config_hex)?)?;
        for (id, expected) in &self.objects {
            let oid = ObjectId::from_hex(id.as_bytes())?;
            if oid.algo() != algo || object_identity(odb, oid)? != *expected {
                return Err(format!("baseline object differs: {id}").into());
            }
        }
        let graph = closure(
            odb,
            self.objects
                .keys()
                .map(|id| ObjectId::from_hex(id.as_bytes()))
                .collect::<std::result::Result<Vec<_>, _>>()?,
        )?;
        if graph.keys().any(|id| !self.objects.contains_key(id)) {
            return Err("baseline object graph leaves the original inventory".into());
        }
        for (name, value) in &self.refs {
            alt_input::checked_path(name.as_bytes())?;
            if name != "HEAD" && !public_ref(name) {
                return Err("non-public baseline ref".into());
            }
            let oid = resolve(&self.refs, name)?;
            if !self.objects.contains_key(&oid.to_string()) {
                return Err("baseline ref object missing".into());
            }
            if value.is_empty() {
                return Err("empty baseline ref".into());
            }
        }
        if !self.refs.contains_key("HEAD") {
            return Err("baseline HEAD missing".into());
        }
        Ok(())
    }
}
pub fn public_ref(name: &str) -> bool {
    ["refs/heads/", "refs/tags/", "refs/remotes/", "refs/notes/"]
        .iter()
        .any(|p| name.starts_with(p))
}
pub fn render(target: &RefTarget) -> String {
    match target {
        RefTarget::Oid(oid) => oid.to_string(),
        RefTarget::Symbolic(name) => format!("ref: {name}"),
    }
}
pub fn resolve(refs: &BTreeMap<String, String>, name: &str) -> Result<ObjectId> {
    let mut name = name;
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(name) {
            return Err("symbolic ref cycle".into());
        }
        let value = refs.get(name).ok_or("missing symbolic ref target")?;
        if let Some(next) = value.strip_prefix("ref: ") {
            name = next;
        } else {
            return Ok(ObjectId::from_hex(value.as_bytes())?);
        }
    }
}
pub fn object_identity(odb: &NativeOdb, oid: ObjectId) -> Result<ObjectIdentity> {
    let object = odb.get(&oid)?.ok_or("missing canonical object")?;
    if ObjectId::hash_object(oid.algo(), object.kind, &object.data) != oid {
        return Err("canonical object hash mismatch".into());
    }
    Ok(ObjectIdentity {
        kind: format!("{}", object.kind),
        bytes: object.data.len() as u64,
        blake3: alt_hash::blake3::hash(&object.data).to_hex().to_string(),
    })
}
pub fn snapshot(
    odb: &NativeOdb,
    refs: BTreeMap<String, String>,
    config: &[u8],
    algo: HashAlgo,
) -> Result<Snapshot> {
    validate_config(config)?;
    let objects = odb
        .entries()
        .map(|entry| Ok((entry.git.to_string(), object_identity(odb, entry.git)?)))
        .collect::<Result<_>>()?;
    let snapshot = Snapshot {
        schema_version: 1,
        object_format: match algo {
            HashAlgo::Sha1 => "sha1",
            HashAlgo::Sha256 => "sha256",
        }
        .into(),
        objects,
        refs,
        config_hex: alt_input::hex(config),
    };
    snapshot.validate(odb)?;
    Ok(snapshot)
}
/// Compute and verify the complete graph. Gitlinks are not execution inputs.
pub fn closure(
    odb: &NativeOdb,
    roots: impl IntoIterator<Item = ObjectId>,
) -> Result<BTreeMap<String, ObjectIdentity>> {
    let mut queue: Vec<_> = roots.into_iter().collect();
    let mut result = BTreeMap::new();
    while let Some(oid) = queue.pop() {
        if result.contains_key(&oid.to_string()) {
            continue;
        }
        let identity = object_identity(odb, oid)?;
        let object = odb.get(&oid)?.ok_or("missing graph object")?;
        match object.kind {
            ObjectKind::Commit => {
                let c = Commit::parse(&object.data)?;
                queue.push(c.tree().ok_or("commit tree missing")?);
                queue.extend(c.parents());
            }
            ObjectKind::Tree => {
                for e in Tree::parse(&object.data, oid.algo())?.entries {
                    if e.mode.is_gitlink() {
                        return Err(
                            "gitlinks require external input and are unsupported for tasks".into(),
                        );
                    }
                    queue.push(e.oid);
                }
            }
            ObjectKind::Tag => queue.push(
                Tag::parse(&object.data)?
                    .object()
                    .ok_or("tag target missing")?,
            ),
            ObjectKind::Blob => {}
        }
        result.insert(oid.to_string(), identity);
    }
    Ok(result)
}
pub fn files(odb: &NativeOdb, commit: ObjectId) -> Result<BTreeMap<String, FileEntry>> {
    let c = odb.get(&commit)?.ok_or("checkout commit missing")?;
    if c.kind != ObjectKind::Commit {
        return Err("checkout requires a commit".into());
    }
    let root = Commit::parse(&c.data)?
        .tree()
        .ok_or("commit tree missing")?;
    let mut result = BTreeMap::new();
    let mut queue = vec![(String::new(), root)];
    while let Some((prefix, oid)) = queue.pop() {
        let tree = odb.get(&oid)?.ok_or("checkout tree missing")?;
        if tree.kind != ObjectKind::Tree {
            return Err("invalid tree edge".into());
        }
        for entry in Tree::parse(&tree.data, oid.algo())?.entries {
            let name = alt_input::checked_path(entry.name.as_slice())?;
            if name.contains('/') {
                return Err("tree entry contains slash".into());
            }
            let path = if prefix.is_empty() {
                name.into()
            } else {
                format!("{prefix}/{name}")
            };
            alt_input::checked_path(path.as_bytes())?;
            if path
                .split('/')
                .any(|p| p.eq_ignore_ascii_case(".git") || p.eq_ignore_ascii_case(".alt"))
            {
                return Err("repository control path in source tree".into());
            }
            if entry.mode.is_tree() {
                queue.push((path, entry.oid));
            } else {
                if !matches!(entry.mode.value(), 0o100644 | 0o100755 | 0o120000) {
                    return Err("unsupported checkout mode".into());
                }
                if result
                    .insert(
                        path,
                        FileEntry {
                            mode: entry.mode.value(),
                            oid: entry.oid.to_string(),
                        },
                    )
                    .is_some()
                {
                    return Err("duplicate tree path".into());
                }
            }
        }
    }
    Ok(result)
}
pub fn config_bytes(alt_dir: &Path) -> Result<Vec<u8>> {
    match fs::read(alt_dir.join("git-import/config")) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n".to_vec())
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests;
