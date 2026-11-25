//! What a publication that is committed leaves behind: its receipt, and the
//! content store without what the segments hold now.

use super::PublicationWork;
use crate::Result;
use crate::database::Transaction;
use alt_crypto::ed25519::SigningKey;
use alt_remote_protocol::{Receipt, ReceiptBody};
use alt_repository_service::ValidatedPublication;
use std::collections::BTreeSet;

/// The receipt of the publication of `work`, validated as `validated`, that is
/// the transaction `sequence` of its repository: signed with the key of the
/// instance.
pub(super) fn receipt(
    key: &SigningKey,
    work: &PublicationWork,
    validated: ValidatedPublication,
    sequence: u64,
    committed_at_unix_ms: u64,
) -> Result<Receipt> {
    let body = ReceiptBody {
        publication: work.publication.clone(),
        request_digest: validated.digest,
        transaction_sequence: sequence,
        policy_version: validated.policy_version,
        committed_at_unix_ms,
    };
    let signature = key.sign(&body.signing_bytes_of_validated()?);
    Ok(Receipt {
        body,
        signing_key_id: alt_hash::blake3::hash(key.verifying_key().as_bytes())
            .to_hex()
            .to_string(),
        signature: signature
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    })
}

/// With the upload pins gone, what this request uploaded and nothing retains
/// leaves the content store: its Git objects, which the segments hold now, and
/// the chunks its content was assembled from. The new segment is counted instead.
pub(super) fn content(
    tx: &mut Transaction<'_>,
    work: &PublicationWork,
    sequence: u64,
    materialized: Option<&crate::packs::Materialized>,
) -> Result<()> {
    let repository = &work.key.scope.repository_instance_id;
    let uploaded: BTreeSet<&str> = work
        .manifest
        .entries
        .iter()
        .flat_map(|entry| {
            std::iter::once(entry.blake3.as_str())
                .chain(entry.chunks.iter().map(|chunk| chunk.blake3.as_str()))
        })
        .collect();
    crate::pack_native::release_content(tx, repository, &uploaded.into_iter().collect::<Vec<_>>())?;
    if let Some(segment) = materialized {
        crate::pack_native::account_segment(tx, repository, segment.bytes)?;
        crate::packs::insert_segment(
            tx,
            repository,
            sequence,
            &crate::NewSegment {
                id: &segment.id,
                objects: segment.objects,
                bytes: segment.bytes,
                flags: segment.flags,
                inline: segment.inline.as_deref(),
            },
        )?;
    }
    Ok(())
}
