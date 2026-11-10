use crate::{Error, Result, checked_path, identity};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Symlink,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub kind: EntryKind,
    pub executable: bool,
    pub len: u64,
    pub content: String,
    pub link_target: Option<String>,
}
impl Node {
    pub fn directory() -> Self {
        Self {
            kind: EntryKind::Directory,
            executable: false,
            len: 0,
            content: alt_hash::blake3::hash(b"").to_hex().to_string(),
            link_target: None,
        }
    }
    pub fn validate(&self, path: &str) -> Result<()> {
        if self.content.len() != 64
            || !self
                .content
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(Error::Integrity(format!("invalid content digest: {path}")));
        }
        match self.kind {
            EntryKind::Symlink => {
                let target = crate::unhex(
                    self.link_target
                        .as_deref()
                        .ok_or_else(|| Error::Integrity("missing link target".into()))?,
                )?;
                checked_link(path, &target)?;
                if self.executable
                    || self.len != target.len() as u64
                    || self.content != alt_hash::blake3::hash(&target).to_hex().as_str()
                {
                    return Err(Error::Integrity(format!("invalid link: {path}")));
                }
            }
            EntryKind::Directory => {
                if self != &Self::directory() {
                    return Err(Error::Integrity(format!("invalid directory: {path}")));
                }
            }
            EntryKind::File => {
                if self.link_target.is_some() {
                    return Err(Error::Integrity(format!("file has link target: {path}")));
                }
            }
        }
        Ok(())
    }
}

pub trait Tree {
    fn inspect(&self, path: &str) -> Result<Node>;
    /// Full relative paths, not just basenames. Links must never be traversed.
    fn children(&self, path: &str) -> Result<Vec<String>>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputRole {
    Source,
    Auxiliary,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub path_bytes_hex: String,
    pub role: InputRole,
    pub node: Node,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRoot {
    pub path: String,
    pub purpose: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputPlan {
    /// Complete index-selected source file set, supplied by the VCS adapter.
    pub source: Vec<String>,
    /// Explicit roots are traversed anew each time, including new members.
    pub auxiliary: Vec<InputRoot>,
    /// Exclusions are explicit and bound into the identity; bytes are not read.
    pub exclusions: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub values: BTreeMap<String, String>,
    pub complete: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolIdentity {
    pub name: String,
    pub version: String,
    pub binary_blake3: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalDependency {
    pub provider: String,
    pub version: Option<String>,
    pub purpose: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub commands: Vec<Vec<String>>,
    pub cwd: String,
    pub environment: Environment,
    pub tools: Vec<ToolIdentity>,
    pub external: Vec<ExternalDependency>,
    /// Immutable acceptance definition; this is part of the input identity.
    pub validation: serde_json::Value,
}
impl Context {
    pub fn uncertainty(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if !self.environment.complete {
            reasons.push("environment_incomplete".into());
        }
        if self.tools.is_empty() {
            reasons.push("tools_missing".into());
        }
        for tool in &self.tools {
            if tool.version.is_empty()
                || tool
                    .binary_blake3
                    .as_ref()
                    .is_none_or(|id| id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                reasons.push(format!("tool_unknown:{}", tool.name));
            }
        }
        for dependency in &self.external {
            if dependency.provider.is_empty()
                || dependency.version.as_ref().is_none_or(String::is_empty)
            {
                reasons.push(format!("external_unknown:{}", dependency.provider));
            }
        }
        reasons
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub hash_algorithm: String,
    pub plan: InputPlan,
    pub context: Context,
    pub entries: Vec<Entry>,
}
impl Manifest {
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        identity("manifest", self)
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.hash_algorithm != "blake3" {
            return Err(Error::Unsupported(
                "input manifest version/algorithm".into(),
            ));
        }
        validate_plan(&self.plan)?;
        for (name, value) in &self.context.environment.values {
            let upper = name.to_ascii_uppercase();
            if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
                return Err(Error::Unsupported(
                    "invalid declared environment entry".into(),
                ));
            }
            if [
                "TOKEN",
                "PASSWORD",
                "PASSWD",
                "SECRET",
                "PRIVATE_KEY",
                "CREDENTIAL",
                "AUTHORIZATION",
                "API_KEY",
                "ACCESS_KEY",
            ]
            .iter()
            .any(|part| upper.contains(part))
            {
                return Err(Error::Unsupported(format!(
                    "credential environment {name} requires an external provider/version reference, not a stored value"
                )));
            }
        }
        if self.context.cwd != "." {
            checked_path(self.context.cwd.as_bytes())?;
        }
        if self.context.commands.is_empty()
            || self.context.commands.iter().any(|command| {
                command.is_empty()
                    || command[0].is_empty()
                    || command.iter().any(|arg| arg.contains('\0'))
            })
        {
            return Err(Error::Unsupported(
                "missing or invalid validation command".into(),
            ));
        }
        let mut last: Option<&str> = None;
        let mut paths = BTreeMap::new();
        for entry in &self.entries {
            checked_path(entry.path.as_bytes())?;
            if self
                .plan
                .exclusions
                .iter()
                .any(|excluded| covered(&entry.path, excluded))
            {
                return Err(Error::Integrity("excluded input was captured".into()));
            }
            let selected = match entry.role {
                InputRole::Source => self.plan.source.contains(&entry.path),
                InputRole::Auxiliary => self
                    .plan
                    .auxiliary
                    .iter()
                    .any(|root| covered(&entry.path, &root.path)),
            };
            if !selected {
                return Err(Error::Integrity(
                    "entry is not declared by input plan".into(),
                ));
            }
            if entry.path_bytes_hex != crate::hex(entry.path.as_bytes())
                || last.is_some_and(|last| last >= entry.path.as_str())
            {
                return Err(Error::Integrity(
                    "unsorted, duplicate or lossy input path".into(),
                ));
            }
            last = Some(&entry.path);
            entry.node.validate(&entry.path)?;
            paths.insert(entry.path.as_str(), &entry.node);
        }
        for path in &self.plan.source {
            if !self
                .entries
                .iter()
                .any(|entry| &entry.path == path && entry.role == InputRole::Source)
            {
                return Err(Error::Integrity(format!("missing source entry: {path}")));
            }
        }
        for root in &self.plan.auxiliary {
            if !self
                .plan
                .exclusions
                .iter()
                .any(|excluded| covered(&root.path, excluded))
                && !paths.contains_key(root.path.as_str())
            {
                return Err(Error::Integrity(format!(
                    "missing auxiliary root: {}",
                    root.path
                )));
            }
        }
        for entry in &self.entries {
            for (at, _) in entry.path.match_indices('/') {
                if paths
                    .get(&entry.path[..at])
                    .is_some_and(|node| node.kind != EntryKind::Directory)
                {
                    return Err(Error::Integrity(format!(
                        "input parent is not a directory: {}",
                        entry.path
                    )));
                }
            }
            if entry.node.kind == EntryKind::Symlink {
                resolve_link(&entry.path, &paths, &mut BTreeSet::new())?;
            }
        }
        Ok(())
    }
}

fn covered(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}
fn validate_plan(plan: &InputPlan) -> Result<()> {
    for path in plan
        .source
        .iter()
        .chain(plan.auxiliary.iter().map(|root| &root.path))
        .chain(&plan.exclusions)
    {
        checked_path(path.as_bytes())?;
        if matches!(path.split('/').next(), Some(".git" | ".alt")) {
            return Err(Error::Unsupported(format!(
                "VCS administration is not an execution input: {path}"
            )));
        }
    }
    for path in &plan.source {
        if plan
            .exclusions
            .iter()
            .any(|excluded| covered(path, excluded))
        {
            return Err(Error::Unsupported(format!(
                "source file is also excluded: {path}"
            )));
        }
    }
    Ok(())
}

pub fn scan(
    source: &dyn Tree,
    auxiliary: &dyn Tree,
    mut plan: InputPlan,
    context: Context,
) -> Result<Manifest> {
    validate_plan(&plan)?;
    plan.source.sort();
    plan.source.dedup();
    plan.auxiliary.sort_by(|a, b| a.path.cmp(&b.path));
    plan.exclusions.sort();
    plan.exclusions.dedup();
    let mut entries = BTreeMap::new();
    for path in &plan.source {
        let node = source.inspect(path)?;
        if node.kind == EntryKind::Directory {
            return Err(Error::Unsupported(format!(
                "source selection must name files: {path}"
            )));
        }
        entries.insert(
            path.clone(),
            Entry {
                path: path.clone(),
                path_bytes_hex: crate::hex(path.as_bytes()),
                role: InputRole::Source,
                node,
            },
        );
    }
    fn walk(
        tree: &dyn Tree,
        path: &str,
        plan: &InputPlan,
        entries: &mut BTreeMap<String, Entry>,
    ) -> Result<()> {
        if plan
            .exclusions
            .iter()
            .any(|excluded| covered(path, excluded))
        {
            return Ok(());
        }
        checked_path(path.as_bytes())?;
        if matches!(path.split('/').next(), Some(".git" | ".alt")) {
            return Err(Error::Unsupported(format!(
                "active VCS administration: {path}"
            )));
        }
        let node = tree.inspect(path)?;
        if entries.contains_key(path) {
            return Err(Error::Unsupported(format!(
                "overlapping source/auxiliary roots: {path}"
            )));
        }
        let directory = node.kind == EntryKind::Directory;
        entries.insert(
            path.into(),
            Entry {
                path: path.into(),
                path_bytes_hex: crate::hex(path.as_bytes()),
                role: InputRole::Auxiliary,
                node,
            },
        );
        if directory {
            let before = tree.children(path)?;
            for child in &before {
                if child.rsplit_once('/').map(|(parent, _)| parent) != Some(path) {
                    return Err(Error::Integrity("tree returned non-child path".into()));
                }
                walk(tree, child, plan, entries)?;
            }
            if before != tree.children(path)? {
                return Err(Error::Changed(path.into()));
            }
        }
        Ok(())
    }
    for root in &plan.auxiliary {
        walk(auxiliary, &root.path, &plan, &mut entries)?;
    }
    let manifest = Manifest {
        schema_version: 1,
        hash_algorithm: "blake3".into(),
        plan,
        context,
        entries: entries.into_values().collect(),
    };
    manifest.validate()?;
    Ok(manifest)
}

pub(crate) fn checked_link(path: &str, target: &[u8]) -> Result<String> {
    let target = std::str::from_utf8(target).map_err(|_| Error::Path {
        reason: "non-UTF-8 link",
        bytes_hex: crate::hex(target),
    })?;
    if target.is_empty() || target.starts_with('/') || target.contains(['\0', '\r', '\n', '\\']) {
        return Err(Error::Unsupported(format!("unsafe link target: {path}")));
    }
    let mut parts: Vec<_> = path.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(Error::Unsupported(format!(
                        "link escapes input root: {path}"
                    )));
                }
            }
            part => {
                checked_path(part.as_bytes())?;
                parts.push(part);
            }
        }
    }
    Ok(parts.join("/"))
}
fn resolve_link(
    path: &str,
    paths: &BTreeMap<&str, &Node>,
    visiting: &mut BTreeSet<String>,
) -> Result<()> {
    if !visiting.insert(path.into()) {
        return Err(Error::Unsupported(format!("cyclic input link: {path}")));
    }
    let node = paths[path];
    let raw_target = crate::unhex(node.link_target.as_deref().expect("validated link"))?;
    let raw_target = std::str::from_utf8(&raw_target).expect("validated UTF-8 link");
    let mut components: Vec<_> = path.split('/').collect();
    components.pop();
    let pieces: Vec<_> = raw_target.split('/').collect();
    for (at, piece) in pieces.iter().enumerate() {
        match *piece {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            part => {
                components.push(part);
            }
        }
        if at + 1 < pieces.len()
            && paths
                .get(components.join("/").as_str())
                .is_some_and(|node| node.kind != EntryKind::Directory)
        {
            return Err(Error::Unsupported(format!(
                "link traverses non-directory before normalization: {path}"
            )));
        }
    }
    let target = checked_link(
        path,
        &crate::unhex(node.link_target.as_deref().expect("validated link"))?,
    )?;
    // Reject links through another link's directory: this avoids lexical '..'
    // checks being defeated by a chain whose filesystem resolution differs.
    for (at, _) in target.match_indices('/') {
        if paths
            .get(&target[..at])
            .is_some_and(|node| node.kind != EntryKind::Directory)
        {
            return Err(Error::Unsupported(format!(
                "link target traverses non-directory: {path}"
            )));
        }
    }
    if paths
        .get(target.as_str())
        .is_some_and(|node| node.kind == EntryKind::Symlink)
    {
        resolve_link(&target, paths, visiting)?;
    }
    visiting.remove(path);
    Ok(())
}
