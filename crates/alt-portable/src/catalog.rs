use crate::{Error, Result};
use alt_provenance::decode_hex;
use alt_remote_protocol::{
    ContentKind, DeploymentMode, Limits, MAX_TRANSFER_PROOFS, Manifest, Publication, Receipt,
    RequestKey, Scope, TransferProof,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};

pub const VERSION: u32 = 1;

/// Per-import resource budgets supplied by the caller, never by the archive.
#[derive(Clone, Copy, Debug)]
pub struct PackageLimits {
    pub metadata_bytes: usize,
    pub entries: usize,
    pub publications: usize,
    pub blob_bytes: u64,
    pub total_bytes: u64,
    pub protocol: Limits,
}
impl PackageLimits {
    pub fn validate(self) -> Result<()> {
        self.protocol.validate()?;
        if self.metadata_bytes == 0
            || self.entries == 0
            || self.publications == 0
            || self.blob_bytes == 0
            || self.total_bytes < self.blob_bytes
        {
            return Err(Error::Limit("invalid package limits"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Blob {
    pub blake3: String,
    #[json(with = "alt_json::with::decimal_u64")]
    pub bytes: u64,
}
impl Blob {
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            blake3: alt_hash::blake3::hash(bytes).to_hex().to_string(),
            bytes: bytes.len() as u64,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Object {
    pub kind: ContentKind,
    pub oid: String,
    pub payload: Blob,
}
#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Reference {
    pub name: String,
    pub oid: String,
}

/// The exact raw metadata payloads remain addressable. Decoding and re-encoding
/// these documents during import would lose the original byte representation.
#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct HistoricalPublication {
    pub digest: String,
    pub publication: Blob,
    pub manifest: Blob,
    pub receipt: Blob,
    pub proofs: Vec<TransferProof>,
}

/// Source permissions explain which identities need an explicit disposition.
/// They are historical requirements, not grants to install on the target.
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct IdentityRequirement {
    pub installation_id: String,
    pub actor_id: String,
    pub had_read: bool,
    pub had_publish_records: bool,
    pub had_review: bool,
    pub ref_grants: Vec<alt_remote_protocol::authorization::Grant>,
}

/// Closed allowlist. There are no arbitrary SQL tables or configuration maps.
/// Repository content may itself contain secrets; the export permission and
/// content selection are the caller's responsibility.
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Catalog {
    pub version: u32,
    pub source: Scope,
    pub source_mode: DeploymentMode,
    pub source_public_key: String,
    pub export_id: String,
    #[json(with = "alt_json::with::decimal_u64")]
    pub snapshot_sequence: u64,
    pub object_format: alt_remote_protocol::ObjectFormat,
    pub default_branch: String,
    pub protections: Vec<alt_repository_service::RefPolicy>,
    pub refs: Vec<Reference>,
    pub objects: Vec<Object>,
    pub publications: Vec<HistoricalPublication>,
    /// Original signed import receipts preserve previous identity dispositions.
    /// These are historical evidence, never current target authorization.
    #[json(default, skip_serializing_if = "Vec::is_empty")]
    pub import_receipts: Vec<Blob>,
    /// Signed cross-product history only. Embedded public keys never install a
    /// live channel, credential, task claim, or current approval on the target.
    #[json(
        default,
        skip_serializing_if = "Vec::is_empty",
        with = "alt_json_serde::bridge"
    )]
    pub change_links: Vec<golia_change_link::HistoricalEvent>,
    pub identities: Vec<IdentityRequirement>,
    pub blobs: Vec<Blob>,
}

fn ordered<T: Ord>(values: impl Iterator<Item = T>) -> Result<()> {
    let mut previous = None;
    for value in values {
        if previous.is_some_and(|p| p >= value) {
            return Err(Error::Invalid("duplicate or unordered catalog identity"));
        }
        previous = Some(value);
    }
    Ok(())
}

impl Catalog {
    pub fn validate(&self, limits: PackageLimits) -> Result<()> {
        limits.validate()?;
        if self.version != VERSION {
            return Err(Error::Invalid("catalog version"));
        }
        self.source.validate()?;
        decode_hex::<32>(&self.source_public_key)?;
        RequestKey {
            scope: self.source.clone(),
            actor_id: "exporter".into(),
            request_id: self.export_id.clone(),
        }
        .validate()?;
        alt_remote_protocol::validate_ref_name(&self.default_branch)?;
        if !self.default_branch.starts_with("refs/heads/") {
            return Err(Error::Invalid("default branch namespace"));
        }
        if self.refs.len() > limits.entries
            || self.protections.len() > limits.entries
            || self.objects.len() > limits.entries
            || self.identities.len() > limits.entries
            || self.blobs.len() > limits.entries
            || self.publications.len() > limits.publications
            || self.import_receipts.len() > limits.entries
            || self.change_links.len() > limits.entries
        {
            return Err(Error::Limit("catalog entries"));
        }
        ordered(self.refs.iter().map(|v| v.name.as_str()))?;
        ordered(self.objects.iter().map(|v| v.oid.as_str()))?;
        ordered(self.publications.iter().map(|v| v.digest.as_str()))?;
        ordered(
            self.identities
                .iter()
                .map(|v| (&v.installation_id, &v.actor_id)),
        )?;
        ordered(self.blobs.iter().map(|v| v.blake3.as_str()))?;
        ordered(self.import_receipts.iter().map(|v| v.blake3.as_str()))?;
        let mut previous_link = None;
        for history in &self.change_links {
            history
                .validate()
                .map_err(|_| Error::Invalid("change-link history"))?;
            let digest = history
                .statement
                .digest()
                .map_err(|_| Error::Invalid("change-link identity"))?;
            if previous_link
                .as_ref()
                .is_some_and(|previous| previous >= &digest)
            {
                return Err(Error::Invalid("duplicate or unordered change-link history"));
            }
            previous_link = Some(digest);
        }
        let mut bytes = self.change_link_bytes()?;
        if bytes > limits.total_bytes {
            return Err(Error::Limit("archive bytes"));
        }
        let mut blobs = BTreeMap::new();
        for blob in &self.blobs {
            decode_hex::<32>(&blob.blake3)?;
            if blob.bytes > limits.blob_bytes {
                return Err(Error::Limit("blob bytes"));
            }
            bytes = bytes
                .checked_add(blob.bytes)
                .ok_or(Error::Limit("archive bytes"))?;
            if bytes > limits.total_bytes {
                return Err(Error::Limit("archive bytes"));
            }
            blobs.insert(blob.blake3.as_str(), blob.bytes);
        }
        let required = |blob: &Blob| -> Result<()> {
            if blobs.get(blob.blake3.as_str()) != Some(&blob.bytes) {
                return Err(Error::Invalid("catalog payload absent or size mismatch"));
            }
            Ok(())
        };
        let mut objects = BTreeMap::new();
        for object in &self.objects {
            self.object_format.validate_oid(&object.oid)?;
            if !matches!(
                object.kind,
                ContentKind::Commit | ContentKind::Tree | ContentKind::Blob | ContentKind::Tag
            ) {
                return Err(Error::Invalid("Git object kind"));
            }
            required(&object.payload)?;
            objects.insert(object.oid.as_str(), object.kind);
        }
        for reference in &self.refs {
            alt_remote_protocol::validate_ref_name(&reference.name)?;
            self.object_format.validate_oid(&reference.oid)?;
            let kind = objects
                .get(reference.oid.as_str())
                .ok_or(Error::Invalid("ref object absent"))?;
            if reference.name.starts_with("refs/heads/") && *kind != ContentKind::Commit {
                return Err(Error::Invalid("branch target kind"));
            }
        }
        for item in &self.publications {
            decode_hex::<32>(&item.digest)?;
            for blob in [&item.publication, &item.manifest, &item.receipt] {
                required(blob)?;
                if blob.bytes > limits.metadata_bytes as u64 {
                    return Err(Error::Limit("publication metadata bytes"));
                }
            }
            if item.proofs.len() > MAX_TRANSFER_PROOFS {
                return Err(Error::Limit("provenance chain"));
            }
            for proof in &item.proofs {
                proof.validate()?;
            }
        }
        for blob in &self.import_receipts {
            required(blob)?;
            if blob.bytes > limits.metadata_bytes as u64 {
                return Err(Error::Limit("import receipt metadata bytes"));
            }
        }
        for identity in &self.identities {
            let mut scope = self.source.clone();
            scope.installation_id = identity.installation_id.clone();
            RequestKey {
                scope,
                actor_id: identity.actor_id.clone(),
                request_id: "identity-mapping".into(),
            }
            .validate()?;
            if identity.ref_grants.len() > limits.entries {
                return Err(Error::Limit("historical grants"));
            }
            for grant in &identity.ref_grants {
                grant.validate()?;
                if grant
                    .paths
                    .as_ref()
                    .is_some_and(|paths| paths.len() > limits.protocol.tree_paths)
                {
                    return Err(Error::Limit("historical path grants"));
                }
            }
        }
        for policy in &self.protections {
            policy.validate()?;
        }
        Ok(())
    }
    /// Logical canonical signed metadata, counted separately from file blobs.
    pub fn change_link_bytes(&self) -> Result<u64> {
        self.change_links.iter().try_fold(0u64, |sum, history| {
            sum.checked_add(alt_json::to_vec(&alt_json_serde::Serde(history))?.len() as u64)
                .ok_or(Error::Limit("change-link bytes"))
        })
    }
    pub fn canonical_bytes(&self, limits: PackageLimits) -> Result<Vec<u8>> {
        self.validate(limits)?;
        let bytes = alt_json::to_vec(self)?;
        if bytes.len() > limits.metadata_bytes {
            return Err(Error::Limit("catalog bytes"));
        }
        Ok(bytes)
    }
    pub fn digest(&self, limits: PackageLimits) -> Result<String> {
        let mut hash = alt_hash::blake3::Hasher::new_derive_key("jp.golia.alt.portable-catalog.v1");
        hash.update(&self.canonical_bytes(limits)?);
        Ok(hash.finalize().to_hex().to_string())
    }
}

/// Implementations provide immutable bytes from an isolated staging area or a
/// pinned source snapshot, with cancellation/timeouts appropriate to their I/O.
pub trait BlobSource {
    fn open(&self, blob: &Blob) -> Result<Box<dyn Read + '_>>;
}
pub fn read_metadata(source: &impl BlobSource, blob: &Blob, limit: usize) -> Result<Vec<u8>> {
    if blob.bytes > limit as u64 {
        return Err(Error::Limit("metadata payload bytes"));
    }
    let mut bytes = Vec::new();
    source
        .open(blob)?
        .take(blob.bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if Blob::of(&bytes) != *blob {
        return Err(Error::Invalid("metadata payload identity"));
    }
    Ok(bytes)
}

/// Verifies original signatures, manifests and source attribution. Full Git
/// object/task graph validation is a separate mandatory import stage.
pub fn verify_publications(
    catalog: &Catalog,
    source: &impl BlobSource,
    limits: PackageLimits,
) -> Result<()> {
    catalog.validate(limits)?;
    let key = decode_hex(&catalog.source_public_key)?;
    let blobs: BTreeMap<_, _> = catalog
        .blobs
        .iter()
        .map(|b| (b.blake3.as_str(), b.bytes))
        .collect();
    let objects: BTreeMap<_, _> = catalog
        .objects
        .iter()
        .map(|o| (o.oid.as_str(), o))
        .collect();
    let identities: BTreeSet<_> = catalog
        .identities
        .iter()
        .map(|i| (i.installation_id.as_str(), i.actor_id.as_str()))
        .collect();
    for item in &catalog.publications {
        let publication = Publication::decode(
            &read_metadata(source, &item.publication, limits.metadata_bytes)?,
            limits.protocol,
        )?;
        let manifest = Manifest::decode(
            &read_metadata(source, &item.manifest, limits.metadata_bytes)?,
            limits.protocol,
        )?;
        let receipt: Receipt = alt_json::from_slice(&read_metadata(
            source,
            &item.receipt,
            limits.metadata_bytes,
        )?)?;
        manifest.validate_publication(&publication, limits.protocol)?;
        if publication.digest(limits.protocol)? != item.digest
            || publication.object_format != catalog.object_format
        {
            return Err(Error::Invalid("publication catalog binding"));
        }
        alt_provenance::verify_origin(
            &publication,
            &receipt,
            &item.proofs,
            &catalog.source,
            &key,
            limits.protocol,
        )?;
        let origin = publication.key.scope.installation_id.as_str();
        if !identities.contains(&(origin, publication.key.actor_id.as_str()))
            || publication
                .attribution
                .on_behalf_of
                .as_deref()
                .is_some_and(|id| !identities.contains(&(origin, id)))
        {
            return Err(Error::Invalid("historical actor mapping absent"));
        }
        for entry in &manifest.entries {
            if blobs.get(entry.blake3.as_str()) != Some(&entry.bytes) {
                return Err(Error::Invalid("publication payload absent"));
            }
            if let Some(oid) = &entry.git_oid {
                let object = objects
                    .get(oid.as_str())
                    .ok_or(Error::Invalid("publication object absent"))?;
                if object.kind != entry.kind
                    || object.payload.blake3 != entry.blake3
                    || object.payload.bytes != entry.bytes
                {
                    return Err(Error::Invalid("publication object identity"));
                }
            }
            verify_chunks(source, entry)?;
        }
    }
    Ok(())
}

fn verify_chunks(
    source: &impl BlobSource,
    entry: &alt_remote_protocol::ContentEntry,
) -> Result<()> {
    let blob = Blob {
        blake3: entry.blake3.clone(),
        bytes: entry.bytes,
    };
    let mut reader = source.open(&blob)?;
    let mut whole = alt_hash::blake3::Hasher::new();
    let mut buffer = [0; 65536];
    for chunk in &entry.chunks {
        let mut hash = alt_hash::blake3::Hasher::new();
        let mut remaining = chunk.bytes;
        while remaining > 0 {
            let n = reader.read(&mut buffer[..remaining.min(65536) as usize])?;
            if n == 0 {
                return Err(Error::Invalid("truncated publication chunk"));
            }
            hash.update(&buffer[..n]);
            whole.update(&buffer[..n]);
            remaining -= n as u64;
        }
        if hash.finalize().to_hex().as_str() != chunk.blake3 {
            return Err(Error::Invalid("publication chunk digest"));
        }
    }
    if reader.read(&mut [0])? != 0 || whole.finalize().to_hex().as_str() != blob.blake3 {
        return Err(Error::Invalid("publication payload identity"));
    }
    Ok(())
}
