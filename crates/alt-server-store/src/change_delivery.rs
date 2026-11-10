//! Project immutable delivery identity from already committed native records.
//! The queue transaction must recheck current authority and repository epoch.
use crate::{Credential, Error, Result, Store};
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_remote_protocol::{RecordKind, RecordRoot, Scope};
use alt_task::Record;
use alt_task_service::TaskRecord;
use golia_change_link::{Counter, Delivery, PublicationScope};
use std::io::Read;

impl Store {
    /// Caller supplies only a record ID, never a commit, receipt or task claim.
    /// published_record verifies raw record identity and the signed publication
    /// origin, including migration/recovery lineage. Its publication was fully
    /// validated by the task service before becoming visible.
    pub fn change_link_delivery(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        delivery_id: &str,
    ) -> Result<Delivery> {
        let selected = self.published_record(
            credential,
            scope,
            &RecordRoot {
                kind: RecordKind::Delivery,
                id: delivery_id.into(),
            },
            1024 * 1024,
        )?;
        let TaskRecord::Delivery(delivery) = selected.record else {
            return Err(Error::Integrity("change-link delivery kind"));
        };
        if delivery
            .id()
            .map_err(|_| Error::Integrity("delivery identity"))?
            != delivery_id
        {
            return Err(Error::Integrity("change-link delivery identity"));
        }
        let integration = self.published_record(
            credential,
            scope,
            &RecordRoot {
                kind: RecordKind::Integration,
                id: delivery.integration.clone(),
            },
            1024 * 1024,
        )?;
        let TaskRecord::Integration(integration) = integration.record else {
            return Err(Error::Integrity("change-link integration kind"));
        };
        if integration.task != delivery.task
            || integration
                .id()
                .map_err(|_| Error::Integrity("integration identity"))?
                != delivery.integration
        {
            return Err(Error::Integrity("change-link integration binding"));
        }
        let oid: ObjectId = integration
            .commit
            .parse()
            .map_err(|_| Error::Integrity("change-link commit identity"))?;
        let (kind, bytes, file) =
            self.open_published_object(credential, &scope.repository_instance_id, oid)?;
        if kind != ObjectKind::Commit || bytes > 16 * 1024 * 1024 {
            return Err(Error::Limit("change-link commit projection"));
        }
        let mut raw = Vec::new();
        file.take(bytes + 1).read_to_end(&mut raw)?;
        if raw.len() as u64 != bytes || ObjectId::hash_object(HashAlgo::Sha1, kind, &raw) != oid {
            return Err(Error::Integrity("change-link commit bytes"));
        }
        let native_commit = alt_hash::blake3::hash(&raw).to_hex().to_string();
        let publication = selected.publication;
        let original = &publication.publication.key.scope;
        Ok(Delivery {
            source: PublicationScope {
                installation: original.installation_id.clone(),
                repository: original.repository_instance_id.clone(),
                epoch: Counter(original.recovery_epoch),
            },
            // Canonical decoded records are used consistently here. These
            // digests name the statements, not an arbitrary JSON whitespace
            // rendering and never an unsigned replacement of the receipt.
            provenance_digest: alt_hash::blake3::hash(&serde_json::to_vec(
                &publication.provenance,
            )?)
            .to_hex()
            .to_string(),
            publication_digest: publication.publication.digest(self.limits)?,
            receipt_digest: alt_hash::blake3::hash(&serde_json::to_vec(&publication.receipt)?)
                .to_hex()
                .to_string(),
            task: delivery.task,
            integration: delivery.integration,
            delivery: delivery_id.into(),
            native_commit,
            git_commit: integration.commit,
        })
    }
}
