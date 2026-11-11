//! Versioned portable transfer messages, shared by server and network clients.
//! These facts do not grant permissions or describe ordinary publications.
use crate::{Blob, Envelope, ImportIntent, ImportReceipt};
use alt_remote_protocol::{RequestKey, Scope};

#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct BeginImport {
    pub intent: ImportIntent,
    pub envelope: Envelope,
}

#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportState {
    Receiving {
        digest: String,
        #[json(with = "alt_json::with::decimal_u64")]
        expires_at_unix_ms: u64,
        missing: Vec<Blob>,
    },
    Committed {
        receipt: Box<ImportReceipt>,
    },
    Abandoned {
        digest: String,
    },
    Unknown {
        key: RequestKey,
    },
    NotSeen {
        key: RequestKey,
    },
}

#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExportState {
    Preparing {
        scope: Scope,
        export_id: String,
        catalog_digest: String,
        #[json(with = "alt_json::with::decimal_u64")]
        snapshot_sequence: u64,
    },
    Available {
        envelope: Box<Envelope>,
        /// Current source write barrier, not a claim about the destination.
        frozen: bool,
    },
    Retired {
        scope: Scope,
        export_id: String,
        catalog_digest: String,
    },
    Unknown {
        scope: Scope,
        export_id: String,
    },
    NotSeen {
        scope: Scope,
        export_id: String,
    },
}

/// Explicit operator limits. The sender cannot enlarge destination budgets.
/// Decimal strings keep byte counts exact in browser clients.
#[derive(Clone, Copy, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Bounds {
    pub metadata_bytes: usize,
    pub entries: usize,
    pub publications: usize,
    #[json(with = "alt_json::with::decimal_u64")]
    pub blob_bytes: u64,
    #[json(with = "alt_json::with::decimal_u64")]
    pub total_bytes: u64,
    #[json(with = "alt_json::with::decimal_u64")]
    pub work_bytes: u64,
    pub reads: usize,
    #[json(with = "alt_json::with::decimal_u64")]
    pub import_ttl_ms: u64,
}
impl Bounds {
    pub fn verification(
        self,
        protocol: alt_remote_protocol::Limits,
        task: alt_task_service::TaskLimits,
    ) -> crate::Result<crate::VerificationLimits> {
        let package = crate::PackageLimits {
            metadata_bytes: self.metadata_bytes,
            entries: self.entries,
            publications: self.publications,
            blob_bytes: self.blob_bytes,
            total_bytes: self.total_bytes,
            protocol,
        };
        package.validate()?;
        if self.work_bytes < self.total_bytes
            || self.reads < self.entries
            || self.import_ttl_ms == 0
            || self.import_ttl_ms > 86_400_000
        {
            return Err(crate::Error::Limit("portable verification limits"));
        }
        Ok(crate::VerificationLimits {
            package,
            task,
            work_bytes: self.work_bytes,
            reads: self.reads,
        })
    }
}

#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Capabilities {
    pub version: u32,
    pub installation_id: String,
    pub receipt_public_key: String,
    pub bounds: Bounds,
}

/// Inspection authenticates the exact canonical archive header against a
/// separately supplied source identity and key. It never stages or activates.
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct InspectArchive {
    pub source: Scope,
    pub source_public_key: String,
    pub metadata: String,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct InspectedArchive {
    pub envelope: Envelope,
    pub catalog_digest: String,
}
impl InspectArchive {
    pub fn verify(&self, limits: crate::PackageLimits) -> crate::Result<InspectedArchive> {
        limits.validate()?;
        if self.metadata.len() > limits.metadata_bytes {
            return Err(crate::Error::Limit("archive metadata bytes"));
        }
        let envelope: Envelope = alt_json::from_str(&self.metadata)?;
        let key = alt_provenance::decode_hex(&self.source_public_key)?;
        let catalog_digest = envelope.verify(&self.source, &key, limits)?;
        if envelope.encode(limits)? != self.metadata.as_bytes() {
            return Err(crate::Error::Invalid("noncanonical envelope"));
        }
        Ok(InspectedArchive {
            envelope,
            catalog_digest,
        })
    }
}
