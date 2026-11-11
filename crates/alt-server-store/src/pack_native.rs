//! Natively uploaded Git objects become a pack segment when they are published, and
//! leave the content store in the same transaction: a published Git object lives in
//! the segments alone.

use crate::{
    Error, Result, Store,
    database::{BATCH_ROWS, Transaction},
    packs::{INLINE_SEGMENT_BYTES, Materialized},
    store::{integer, unsigned},
};
use alt_pack_segment::{Builder, Kind, Segment, SegmentSet};
use alt_remote_protocol::{ContentEntry, ContentKind};
use std::{fs::File, path::PathBuf, sync::Arc};

pub(crate) fn segment_kind(kind: ContentKind) -> Option<Kind> {
    match kind {
        ContentKind::Commit => Some(Kind::Commit),
        ContentKind::Tree => Some(Kind::Tree),
        ContentKind::Blob => Some(Kind::Blob),
        ContentKind::Tag => Some(Kind::Tag),
        _ => None,
    }
}

pub(crate) fn segment_key(oid: &str) -> Option<[u8; 20]> {
    let mut key = [0u8; 20];
    if oid.len() != 40 {
        return None;
    }
    for (byte, pair) in key.iter_mut().zip(oid.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

/// Whether the segments hold the Git object this manifest entry names, with the
/// same kind, content digest and size. False for content that is not a Git object.
pub(crate) fn holds_entry(segments: &SegmentSet, entry: &ContentEntry) -> Result<bool> {
    let (Some(kind), Some(key)) = (
        segment_kind(entry.kind),
        entry.git_oid.as_deref().and_then(segment_key),
    ) else {
        return Ok(false);
    };
    Ok(segments
        .record(&key)
        .map_err(|_| Error::Integrity("pack segment record"))?
        .is_some_and(|record| {
            record.kind == kind
                && record.size == entry.bytes
                && alt_provenance::encode_hex(&record.blake3) == entry.blake3
        }))
}

/// a segment being written; its file is removed unless it is finished
pub(crate) struct Materializing<'a> {
    builder: Option<Builder<'a, File>>,
    id: String,
    path: PathBuf,
}

impl Materializing<'_> {
    pub(crate) fn add(&mut self, kind: Kind, data: &[u8]) -> Result<()> {
        self.builder
            .as_mut()
            .expect("unfinished segment")
            .add(kind, data)
            .map_err(invalid)
    }

    pub(crate) fn finish(mut self) -> Result<Materialized> {
        let built = self
            .builder
            .take()
            .expect("unfinished segment")
            .finish()
            .map_err(invalid);
        let result = built.and_then(|built| {
            let inline = built.bytes <= INLINE_SEGMENT_BYTES as u64;
            if !inline {
                built.out.sync_all()?;
                File::open(self.path.parent().expect("segment directory"))?.sync_all()?;
            }
            drop(built.out);
            Ok(Materialized {
                id: self.id.clone(),
                objects: built.objects,
                bytes: built.bytes,
                flags: built.flags,
                inline: inline.then(|| std::fs::read(&self.path)).transpose()?,
                segment: Arc::new(
                    Segment::open(&self.path)
                        .map_err(|_| Error::Integrity("materialized segment"))?,
                ),
                path: self.path.clone(),
            })
        });
        if result.is_err() {
            let _ = std::fs::remove_file(&self.path);
        }
        result
    }
}

impl Drop for Materializing<'_> {
    fn drop(&mut self) {
        if self.builder.is_some() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// the objects handed over are not a closed, well-formed set
pub(crate) const INVALID_SEGMENT: &str = "native Git objects do not form a valid segment";

/// the segment written did not return an object as it was handed over
pub(crate) const UNREADABLE_SEGMENT: &str = "written pack segment does not read back";

fn invalid(error: alt_pack_segment::Error) -> Error {
    match error {
        alt_pack_segment::Error::Io(error) => Error::Io(error),
        alt_pack_segment::Error::Corrupt(alt_pack_segment::UNREADABLE) => {
            Error::Integrity(UNREADABLE_SEGMENT)
        }
        _ => Error::Integrity(INVALID_SEGMENT),
    }
}

impl crate::ByteStore {
    /// Compresses the Git objects written into new pack segments with `compress`
    /// instead of the built-in compressor. A segment is published only after every
    /// object has been read back from it, whatever wrote the streams.
    pub fn packing_with(mut self, compress: alt_pack_segment::Compress) -> Self {
        self.pack_compress = Some(compress);
        self
    }
}

/// Starts a segment of `objects` Git objects in the byte volume. The file is
/// written as they are added; the row that makes it live is the caller's to insert.
pub(crate) fn materialize_at<'a>(
    bytes: &crate::ByteStore,
    repository: &str,
    existing: &'a SegmentSet,
    objects: u64,
) -> Result<Materializing<'a>> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut nonce = [0u8; 16];
    alt_os::random::fill(&mut nonce);
    let id: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let root = bytes.root();
    crate::packs::pack_directory_at(root, repository)?;
    let path = crate::packs::segment_file(root, repository, &id)?;
    // read as well: the builder reads back what it wrote
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let mut writing = Materializing {
        builder: None,
        id,
        path,
    };
    let builder = match bytes.pack_compress {
        Some(compress) => Builder::compressing(file, existing, objects, compress),
        None => Builder::new(file, existing, objects),
    };
    writing.builder = Some(builder.map_err(invalid)?);
    Ok(writing)
}

impl Store {
    pub(crate) fn materialize<'a>(
        &mut self,
        repository: &str,
        existing: &'a SegmentSet,
        objects: u64,
    ) -> Result<Materializing<'a>> {
        materialize_at(&self.bytes, repository, existing, objects)
    }

    /// the live segments of a repository as of now, for a caller that has yet to
    /// authorize whatever it decides from them
    pub(crate) fn published_segments(&mut self, repository: &str) -> Result<SegmentSet> {
        Ok(self.live_segments(repository)?.1)
    }

    /// the live segments with the list they were opened from
    pub(crate) fn live_segments(
        &mut self,
        repository: &str,
    ) -> Result<(Vec<(String, bool)>, SegmentSet)> {
        let mut tx = self.client.transaction()?;
        let ids = Self::live_segment_ids(&mut tx, repository)?;
        tx.commit()?;
        self.open_listed_segments(repository, ids)
    }
}

/// Counts a new segment against the repository's quota, like a pushed pack.
pub(crate) fn account_segment(
    tx: &mut Transaction<'_>,
    repository: &str,
    bytes: u64,
) -> Result<()> {
    if tx.execute(
        "UPDATE alt_repositories SET stored_bytes=stored_bytes+$2 WHERE id=$1 AND $2<=quota_bytes-stored_bytes-reserved_bytes-link_bytes",
        &[&repository, &integer(bytes)?],
    )? != 1
    {
        return Err(Error::Limit("repository quota"));
    }
    Ok(())
}

/// Lets go of content the content store holds for nothing any more: copies of Git
/// objects the segments hold, and the chunks an upload was assembled from. Only
/// content nothing retains goes: a pin of any kind keeps it, as bytes that are also
/// an attachment or another upload in flight have. What is released is an
/// unregistered placeholder again, no longer counted, and collected like an
/// abandoned upload. A Git mapping goes with its content, or once no upload uses it.
pub(crate) fn release_content(
    tx: &mut Transaction<'_>,
    repository: &str,
    digests: &[&str],
) -> Result<()> {
    let mut freed = 0u64;
    for batch in digests.chunks(BATCH_ROWS) {
        let rows = tx.query(
            "WITH released AS (SELECT b.blake3,b.bytes,b.accounted FROM alt_blobs b WHERE b.repository=$1 AND b.blake3=ANY($2) AND b.state='stored' AND NOT EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=b.repository AND p.blake3=b.blake3) FOR UPDATE) UPDATE alt_blobs b SET state='staging',accounted=false FROM released r WHERE b.repository=$1 AND b.blake3=r.blake3 RETURNING r.blake3,r.bytes,r.accounted",
            &[&repository, &batch],
        )?;
        let mut released = Vec::with_capacity(rows.len());
        for row in &rows {
            if row.get::<_, bool>(2) {
                freed = freed
                    .checked_add(unsigned(row.get(1))?)
                    .ok_or(Error::Integrity("released content bytes"))?;
            }
            released.push(row.get::<_, String>(0));
        }
        tx.execute(
            "DELETE FROM alt_blob_locations WHERE repository=$1 AND blake3=ANY($2)",
            &[&repository, &released],
        )?;
        tx.execute(
            "DELETE FROM alt_objects o WHERE o.repository=$1 AND o.blake3=ANY($2) AND NOT EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=o.repository AND p.blake3=o.blake3 AND p.pin_id LIKE 'upload:%')",
            &[&repository, &batch],
        )?;
    }
    tx.execute(
        "UPDATE alt_repositories SET stored_bytes=stored_bytes-$2 WHERE id=$1",
        &[&repository, &integer(freed)?],
    )?;
    Ok(())
}
