//! The data step of schema 19. Older versions kept every natively published Git
//! object in the content store, retained there by pins of the refs, task bundles,
//! imports and exports that named it, and counted it against the quota. Here those
//! objects move to the pack segments where they are not held already, their copies
//! and pins go, and each repository's stored bytes are counted again.

use crate::{
    ByteStore, Error, Result,
    database::{BATCH_ROWS, Transaction},
    pack_native::{holds_entry, materialize_at, release_content, segment_key, segment_kind},
    packs::{NewSegment, insert_segment, segment_file},
    store::unsigned,
};
use alt_pack_segment::{Segment, SegmentSet};
use alt_portable::{Envelope, HistoricalPublication};
use alt_remote_protocol::{ContentEntry, ContentKind, Manifest, Scope};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::PathBuf,
    sync::Arc,
};

/// Runs inside the migration transaction. Returns the segment files it wrote, which
/// belong to rows of that transaction: the caller removes them if it does not commit.
pub(crate) fn settle(
    tx: &mut Transaction<'_>,
    bytes: &ByteStore,
    installation: &str,
    written: &mut Vec<PathBuf>,
) -> Result<()> {
    let repositories = tx.query(
        "SELECT id,recovery_epoch,transaction_sequence FROM alt_repositories ORDER BY id",
        &[],
    )?;
    for row in repositories {
        let scope = Scope {
            installation_id: installation.into(),
            repository_instance_id: row.get(0),
            recovery_epoch: unsigned(row.get(1))?,
        };
        let sequence = unsigned(row.get(2))?.max(1);
        if let Err(error) = settle_repository(tx, bytes, &scope, sequence, written) {
            alt_server_telemetry::BackgroundLog::current().failed(
                "schema 19 migration",
                &scope.repository_instance_id,
                &error,
            );
            return Err(error);
        }
    }
    // no pin of a ref is left, in any repository
    tx.batch_execute("ALTER TABLE alt_pins VALIDATE CONSTRAINT alt_pins_no_ref")?;
    Ok(())
}

fn object_entry(row: &postgres::Row) -> Result<ContentEntry> {
    let kind = match row.get::<_, String>(1).as_str() {
        "blob" => ContentKind::Blob,
        "tree" => ContentKind::Tree,
        "commit" => ContentKind::Commit,
        "tag" => ContentKind::Tag,
        _ => return Err(Error::Integrity("stored Git object kind")),
    };
    Ok(ContentEntry {
        kind,
        blake3: row.get(2),
        bytes: unsigned(row.get(3))?,
        git_oid: Some(row.get(0)),
        chunks: Vec::new(),
    })
}

fn settle_repository(
    tx: &mut Transaction<'_>,
    bytes: &ByteStore,
    scope: &Scope,
    sequence: u64,
    written: &mut Vec<PathBuf>,
) -> Result<()> {
    let repository = scope.repository_instance_id.as_str();
    let mut segments = live_segments(tx, bytes, repository)?;
    // published Git objects the content store holds: what a permanent pin retains
    let objects = tx
        .query(
            "SELECT o.oid,o.kind,o.blake3,b.bytes FROM alt_objects o JOIN alt_blobs b ON b.repository=o.repository AND b.blake3=o.blake3 WHERE o.repository=$1 AND b.state='stored' AND EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=o.repository AND p.blake3=o.blake3 AND p.expires_at IS NULL) ORDER BY o.oid",
            &[&repository],
        )?
        .iter()
        .map(object_entry)
        .collect::<Result<Vec<_>>>()?;
    let mut absent = Vec::new();
    for entry in &objects {
        let key = entry
            .git_oid
            .as_deref()
            .and_then(segment_key)
            .ok_or(Error::Integrity("stored Git object id"))?;
        if !segments.locate_any(&key) {
            absent.push(entry);
        } else if !holds_entry(&segments, entry)? {
            return Err(Error::Integrity(
                "stored Git object differs from its pack segment",
            ));
        }
    }
    if !absent.is_empty() {
        let digests: Vec<&str> = absent.iter().map(|entry| entry.blake3.as_str()).collect();
        let locations = crate::segments::locations(tx, repository, &digests)?;
        let mut segment = materialize_at(bytes.root(), repository, &segments, absent.len() as u64)?;
        for entry in &absent {
            let mut data = Vec::with_capacity(entry.bytes as usize);
            bytes
                .read_payload(
                    scope,
                    &entry.blake3,
                    entry.bytes,
                    locations.get(&entry.blake3),
                )?
                .read_to_end(&mut data)?;
            segment.add(
                segment_kind(entry.kind).ok_or(Error::Integrity("stored Git object kind"))?,
                &data,
            )?;
        }
        let segment = segment.finish()?;
        written.push(segment.path.clone());
        insert_segment(
            tx,
            repository,
            sequence,
            &NewSegment {
                id: &segment.id,
                objects: segment.objects,
                bytes: segment.bytes,
                flags: segment.flags,
                inline: segment.inline.as_deref(),
            },
        )?;
        let mut all = segments.segments().to_vec();
        all.push(segment.segment);
        segments = SegmentSet::new(all);
        // every object is where readers look for it now, under the id and with the
        // content the content store had for it
        for entry in &absent {
            if !holds_entry(&segments, entry)? {
                return Err(Error::Integrity(
                    "stored Git object differs from its pack segment",
                ));
            }
        }
    }
    let kept = other_uses(tx, repository)?;
    tx.execute(
        "DELETE FROM alt_pins WHERE repository=$1 AND pin_id LIKE 'ref:%'",
        &[&repository],
    )?;
    let git: BTreeSet<&str> = objects.iter().map(|entry| entry.blake3.as_str()).collect();
    let only_git: Vec<&str> = git
        .iter()
        .copied()
        .filter(|digest| !kept.contains(*digest))
        .collect();
    for batch in only_git.chunks(BATCH_ROWS) {
        tx.execute(
            "DELETE FROM alt_pins WHERE repository=$1 AND blake3=ANY($2) AND expires_at IS NULL AND (pin_id LIKE 'task-bundle:%' OR pin_id LIKE 'portable-import:%' OR pin_id LIKE 'portable-export:%')",
            &[&repository, &batch],
        )?;
    }
    release_content(tx, repository, &git.into_iter().collect::<Vec<_>>())?;
    tx.execute(
        "UPDATE alt_repositories SET stored_bytes=(SELECT COALESCE(sum(bytes),0) FROM alt_pack_segments WHERE repository=$1 AND state='live')+(SELECT COALESCE(sum(bytes),0) FROM alt_blobs WHERE repository=$1 AND accounted) WHERE id=$1",
        &[&repository],
    )?;
    Ok(())
}

/// Content digests the content store has to keep whatever Git object shares their
/// bytes: non-Git entries of retained publications, the documents and receipts of
/// imports, and what exports that are still open serve besides Git objects.
fn other_uses(tx: &mut Transaction<'_>, repository: &str) -> Result<BTreeSet<String>> {
    let mut kept = BTreeSet::new();
    let manifest = |kept: &mut BTreeSet<String>, value: serde_json::Value| -> Result<()> {
        let manifest: Manifest = serde_json::from_value(value)?;
        kept.extend(
            manifest
                .entries
                .into_iter()
                .filter(|entry| entry.git_oid.is_none())
                .map(|entry| entry.blake3),
        );
        Ok(())
    };
    for row in tx.query("SELECT q.manifest FROM alt_requests q JOIN alt_task_bundles b ON b.repository=q.repository AND b.epoch=q.epoch AND b.actor_id=q.actor_id AND b.request_id=q.request_id WHERE q.repository=$1 AND q.state='committed'", &[&repository])? {
        manifest(&mut kept, row.get(0))?;
    }
    for row in tx.query(
        "SELECT manifest,original FROM alt_archived_publications WHERE repository=$1",
        &[&repository],
    )? {
        manifest(&mut kept, row.get(0))?;
        let original: HistoricalPublication = serde_json::from_value(row.get(1))?;
        for blob in [original.publication, original.manifest, original.receipt] {
            kept.insert(blob.blake3);
        }
    }
    let envelopes = tx.query("SELECT envelope,NULL::jsonb FROM alt_portable_imports WHERE repository=$1 AND state='committed' UNION ALL SELECT envelope,documents FROM alt_portable_exports WHERE repository=$1 AND NOT retired", &[&repository])?;
    for row in envelopes {
        let envelope: Envelope = serde_json::from_value(row.get(0))?;
        // an export serves its own documents; they are not content-store bytes
        let documents: BTreeMap<String, serde_json::Value> = row
            .get::<_, Option<serde_json::Value>>(1)
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let catalog = envelope.catalog;
        let objects: BTreeSet<&str> = catalog
            .objects
            .iter()
            .map(|object| object.payload.blake3.as_str())
            .collect();
        let history = catalog
            .publications
            .iter()
            .flat_map(|item| [&item.publication, &item.manifest, &item.receipt]);
        for blob in catalog
            .blobs
            .iter()
            .filter(|blob| !objects.contains(blob.blake3.as_str()))
            .chain(&catalog.import_receipts)
            .chain(history)
        {
            if !documents.contains_key(&blob.blake3) {
                kept.insert(blob.blake3.clone());
            }
        }
    }
    Ok(kept)
}

/// The live segments of a repository, opened inside the migration transaction.
fn live_segments(
    tx: &mut Transaction<'_>,
    bytes: &ByteStore,
    repository: &str,
) -> Result<SegmentSet> {
    let rows = tx.query(
        "SELECT segment_id,inline FROM alt_pack_segments WHERE repository=$1 AND state='live' ORDER BY sequence",
        &[&repository],
    )?;
    let mut segments = Vec::with_capacity(rows.len());
    for row in rows {
        let path = segment_file(bytes.root(), repository, &row.get::<_, String>(0))?;
        // the file of an unsettled segment was never flushed; the row is the truth
        if let Some(inline) = row.get::<_, Option<Vec<u8>>>(1) {
            std::fs::create_dir_all(path.parent().expect("segment directory"))?;
            let staging = path.with_extension("restore");
            std::fs::write(&staging, &inline)?;
            std::fs::rename(&staging, &path)?;
        }
        segments.push(Arc::new(
            Segment::open(&path).map_err(|_| Error::Integrity("pack segment unavailable"))?,
        ));
    }
    Ok(SegmentSet::new(segments))
}
