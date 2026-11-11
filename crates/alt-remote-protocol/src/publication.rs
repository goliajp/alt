use crate::{
    Attribution, Error, Limits, ObjectFormat, RequestKey, Result, VERSION, hex, validate_ref_name,
    version,
};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct RefUpdate {
    pub name: String,
    pub expected: Option<String>,
    pub new: Option<String>,
    /// Intent to replace history. This flag does not grant force authority.
    pub force: bool,
}
impl RefUpdate {
    pub fn validate(&self, format: ObjectFormat) -> Result<()> {
        validate_ref_name(&self.name)?;
        for oid in [&self.expected, &self.new].into_iter().flatten() {
            format.validate_oid(oid)?;
        }
        if self.expected == self.new {
            return Err(Error::Invalid("unchanged reference"));
        }
        Ok(())
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, alt_json::ToJson, alt_json::FromJson,
)]
#[json(rename_all = "snake_case")]
pub enum RecordKind {
    Task,
    Candidate,
    Evidence,
    Review,
    Integration,
    Delivery,
}

impl RecordKind {
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Candidate => "candidate",
            Self::Evidence => "evidence",
            Self::Review => "review",
            Self::Integration => "integration",
            Self::Delivery => "delivery",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct RecordRoot {
    pub kind: RecordKind,
    pub id: String,
}
impl RecordRoot {
    pub fn validate(&self) -> Result<()> {
        hex(&self.id, 64, "record id")
    }
}

/// The selected roots are explicit. Ordinary Git push has no such selection
/// and never implicitly uploads a local private task store.
#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Publication {
    pub protocol_version: u32,
    pub key: RequestKey,
    pub attribution: Attribution,
    pub object_format: ObjectFormat,
    pub refs: Vec<RefUpdate>,
    pub record_roots: Vec<RecordRoot>,
    /// BLAKE3 identity of the canonical transfer manifest, including its kinds,
    /// sizes and content identities. A service validates the actual closure.
    pub content_manifest: String,
}
impl Publication {
    pub fn decode(bytes: &[u8], limits: Limits) -> Result<Self> {
        limits.validate()?;
        if bytes.len() > limits.request_bytes {
            return Err(Error::Limit("request bytes"));
        }
        let value: Self = alt_json::from_slice(bytes).map_err(|e| Error::Json(e.to_string()))?;
        value.validate(limits)?;
        Ok(value)
    }
    pub fn validate(&self, limits: Limits) -> Result<()> {
        limits.validate()?;
        version(self.protocol_version)?;
        self.key.validate()?;
        self.attribution.validate()?;
        hex(&self.content_manifest, 64, "content manifest")?;
        if self.refs.len() > limits.ref_updates {
            return Err(Error::Limit("reference updates"));
        }
        if self.record_roots.len() > limits.record_roots {
            return Err(Error::Limit("record roots"));
        }
        if self.refs.is_empty() && self.record_roots.is_empty() {
            return Err(Error::Invalid("empty publication"));
        }
        let mut refs = BTreeSet::new();
        for update in &self.refs {
            update.validate(self.object_format)?;
            if !refs.insert(&update.name) {
                return Err(Error::Invalid("duplicate reference"));
            }
        }
        let mut records = BTreeSet::new();
        for root in &self.record_roots {
            root.validate()?;
            if !records.insert(root) {
                return Err(Error::Invalid("duplicate record root"));
            }
        }
        Ok(())
    }
    /// Version 1 normalizes unordered updates/roots, then encodes this closed
    /// schema as UTF-8 JSON. There are no floats, arbitrary maps, default fields
    /// or ignored extensions. Every accepted request parameter is included.
    pub fn canonical_bytes(&self, limits: Limits) -> Result<Vec<u8>> {
        self.validate(limits)?;
        let mut normalized = self.clone();
        normalized.refs.sort_by(|a, b| a.name.cmp(&b.name));
        normalized
            .record_roots
            .sort_by(|a, b| (a.kind.tag(), &a.id).cmp(&(b.kind.tag(), &b.id)));
        let bytes = alt_json::to_vec(&normalized).map_err(|e| Error::Json(e.to_string()))?;
        if bytes.len() > limits.request_bytes {
            return Err(Error::Limit("request bytes"));
        }
        Ok(bytes)
    }
    pub fn digest(&self, limits: Limits) -> Result<String> {
        let mut hash = alt_hash::blake3::Hasher::new_derive_key("jp.golia.alt.publication.v1");
        hash.update(&self.canonical_bytes(limits)?);
        Ok(hash.finalize().to_hex().to_string())
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, alt_json::ToJson, alt_json::FromJson,
)]
#[json(rename_all = "snake_case")]
pub enum ContentKind {
    Commit,
    Tree,
    Blob,
    Tag,
    Record,
    Attachment,
    TaskBundle,
}
impl ContentKind {
    fn tag(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tree => "tree",
            Self::Blob => "blob",
            Self::Tag => "tag",
            Self::Record => "record",
            Self::Attachment => "attachment",
            Self::TaskBundle => "task_bundle",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Chunk {
    #[json(with = "alt_json::with::decimal_u64")]
    pub offset: u64,
    #[json(with = "alt_json::with::decimal_u64")]
    pub bytes: u64,
    pub blake3: String,
}

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct ContentEntry {
    pub kind: ContentKind,
    /// BLAKE3 of the payload bytes, independent of a Git object's header.
    pub blake3: String,
    #[json(with = "alt_json::with::decimal_u64")]
    pub bytes: u64,
    /// Git kinds require their Git object ID; task/attachment bytes have none.
    pub git_oid: Option<String>,
    pub chunks: Vec<Chunk>,
}
#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Manifest {
    pub protocol_version: u32,
    pub object_format: ObjectFormat,
    pub entries: Vec<ContentEntry>,
}
impl Manifest {
    pub fn decode(bytes: &[u8], limits: Limits) -> Result<Self> {
        limits.validate()?;
        if bytes.len() > limits.request_bytes {
            return Err(Error::Limit("manifest bytes"));
        }
        let value: Self = alt_json::from_slice(bytes).map_err(|e| Error::Json(e.to_string()))?;
        value.validate(limits)?;
        Ok(value)
    }
    pub fn validate(&self, limits: Limits) -> Result<()> {
        limits.validate()?;
        version(self.protocol_version)?;
        if self.entries.len() > limits.manifest_entries {
            return Err(Error::Limit("manifest entries"));
        }
        let mut total = 0u64;
        let mut identities = BTreeSet::new();
        let mut oids = BTreeSet::new();
        for entry in &self.entries {
            hex(&entry.blake3, 64, "content digest")?;
            if entry.bytes > limits.object_bytes {
                return Err(Error::Limit("object bytes"));
            }
            if !matches!(entry.kind, ContentKind::Blob | ContentKind::Attachment)
                && entry.bytes > limits.structured_bytes as u64
            {
                return Err(Error::Limit("structured object bytes"));
            }
            let mut offset = 0u64;
            for chunk in &entry.chunks {
                hex(&chunk.blake3, 64, "chunk digest")?;
                if chunk.offset != offset || chunk.bytes == 0 {
                    return Err(Error::Invalid("chunk range"));
                }
                if chunk.bytes > limits.chunk_bytes {
                    return Err(Error::Limit("chunk bytes"));
                }
                offset = offset
                    .checked_add(chunk.bytes)
                    .ok_or(Error::Limit("object bytes"))?;
            }
            if offset != entry.bytes {
                return Err(Error::Invalid("incomplete chunk manifest"));
            }
            total = total
                .checked_add(entry.bytes)
                .ok_or(Error::Limit("total bytes"))?;
            if total > limits.total_bytes {
                return Err(Error::Limit("total bytes"));
            }
            if !identities.insert((entry.kind, &entry.blake3)) {
                return Err(Error::Invalid("duplicate content entry"));
            }
            match (entry.kind, &entry.git_oid) {
                (ContentKind::Record | ContentKind::Attachment | ContentKind::TaskBundle, None) => {
                }
                (
                    ContentKind::Commit | ContentKind::Tree | ContentKind::Blob | ContentKind::Tag,
                    Some(oid),
                ) => {
                    self.object_format.validate_oid(oid)?;
                    if !oids.insert(oid) {
                        return Err(Error::Invalid("duplicate Git object"));
                    }
                }
                _ => return Err(Error::Invalid("content kind and Git id")),
            }
        }
        Ok(())
    }
    /// Bind an explicit task inventory to this exact publication. Git-only
    /// publications do not implicitly select any task history.
    pub fn validate_publication(&self, publication: &Publication, limits: Limits) -> Result<()> {
        self.validate(limits)?;
        publication.validate(limits)?;
        if self.object_format != publication.object_format
            || self.id(limits)? != publication.content_manifest
        {
            return Err(Error::Invalid("publication manifest binding"));
        }
        let bundles = self
            .entries
            .iter()
            .filter(|e| e.kind == ContentKind::TaskBundle)
            .count();
        if bundles != usize::from(!publication.record_roots.is_empty()) {
            return Err(Error::Invalid("task bundle selection"));
        }
        Ok(())
    }
    pub fn canonical_bytes(&self, limits: Limits) -> Result<Vec<u8>> {
        self.validate(limits)?;
        let mut normalized = self.clone();
        normalized
            .entries
            .sort_by(|a, b| (a.kind.tag(), &a.blake3).cmp(&(b.kind.tag(), &b.blake3)));
        let bytes = alt_json::to_vec(&normalized).map_err(|e| Error::Json(e.to_string()))?;
        if bytes.len() > limits.request_bytes {
            return Err(Error::Limit("manifest bytes"));
        }
        Ok(bytes)
    }
    pub fn id(&self, limits: Limits) -> Result<String> {
        let mut hash =
            alt_hash::blake3::Hasher::new_derive_key("jp.golia.alt.transfer-manifest.v1");
        hash.update(&self.canonical_bytes(limits)?);
        Ok(hash.finalize().to_hex().to_string())
    }
    pub fn empty(format: ObjectFormat) -> Self {
        Self {
            protocol_version: VERSION,
            object_format: format,
            entries: Vec::new(),
        }
    }
}
