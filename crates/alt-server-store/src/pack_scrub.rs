//! Reading every object of a repository's live segments, and writing again the
//! index of a segment that does not give its objects back.
//!
//! A segment is verified once, when it comes in, and trusted from then on. This
//! is the check an operator asks for afterwards: each object is read the way a
//! fetch reads it and held against its id. A segment that fails is replaced, where
//! that is asked for, by one whose index is made anew of the same pack, the way a
//! merge replaces segments; where its pack does not verify either it stays as it
//! is and is told of.

use crate::{
    Credential, Error, Result, Store,
    packs::INLINE_SEGMENT_BYTES,
    store::{instance_unlocked, require_admin},
};
use alt_pack_segment::{Segment, SegmentSet};
use std::sync::Arc;

/// the objects told of by name for one segment; the rest are counted
const NAMED: usize = 16;

/// A live segment that does not give back every object of its index.
pub struct Unread {
    pub segment: String,
    /// how many of its objects are not read
    pub objects: u64,
    /// the first of them, each with what its read ended in
    pub named: Vec<(String, String)>,
    /// the segment that took its place, where one did
    pub replaced_by: Option<String>,
    /// why none did, where that was asked for
    pub kept_because: Option<String>,
}

/// What reading the live segments of one repository found.
pub struct Scrubbed {
    pub repository: String,
    pub segments: u64,
    pub objects: u64,
    pub unread: Vec<Unread>,
}

impl Store {
    /// Writes `content` as the file of a segment that has no row yet, under an id
    /// of its own. One small enough to be held by its row is not flushed.
    pub(crate) fn write_segment(
        &self,
        repository: &str,
        content: &[u8],
    ) -> Result<(String, std::path::PathBuf)> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut nonce = [0u8; 16];
        alt_os::random::fill(&mut nonce);
        let id: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = self.pack_path(repository, &id)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(content)?;
        if content.len() > INLINE_SEGMENT_BYTES {
            file.sync_all()?;
            std::fs::File::open(path.parent().expect("segment directory"))?.sync_all()?;
        }
        Ok((id, path))
    }

    /// The repositories of the instance, in the order of their names.
    pub fn repositories_to_scrub(&mut self, admin: &Credential) -> Result<Vec<String>> {
        let mut tx = self.client.transaction()?;
        instance_unlocked(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        let rows = tx.query("SELECT id FROM alt_repositories ORDER BY id", &[])?;
        tx.commit()?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    /// Reads every object of the live segments of `repository`, oldest segment
    /// first. With `mend`, a segment that does not give back all of its objects is
    /// replaced by its pack indexed anew, and the segments behind it are read
    /// against what replaced it.
    pub fn scrub_packs(
        &mut self,
        admin: &Credential,
        repository: &str,
        mend: bool,
    ) -> Result<Scrubbed> {
        let mut tx = self.client.transaction()?;
        instance_unlocked(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        let ids = Self::live_segment_ids(&mut tx, repository)?;
        tx.commit()?;
        let set = self.open_segments(repository, &ids)?;
        let mut scrubbed = Scrubbed {
            repository: repository.to_owned(),
            segments: ids.len() as u64,
            objects: set.objects(),
            unread: Vec::new(),
        };
        let mut older: Vec<Arc<Segment>> = Vec::with_capacity(ids.len());
        for (part, segment) in ids.iter().zip(set.segments()) {
            let before = SegmentSet::new(older.clone());
            let lost = alt_pack_segment::unread(segment, &before)
                .map_err(|_| Error::Integrity("pack segment index"))?;
            if lost.is_empty() {
                older.push(Arc::clone(segment));
                continue;
            }
            let mut unread = Unread {
                segment: part.0.clone(),
                objects: lost.len() as u64,
                named: (lost.iter().take(NAMED))
                    .map(|(oid, error)| (alt_pack_segment::hex(oid), error.to_string()))
                    .collect(),
                replaced_by: None,
                kept_because: None,
            };
            let mut stands = Arc::clone(segment);
            if mend {
                match self.index_anew(repository, part, segment, &before) {
                    Ok((id, anew)) => {
                        unread.replaced_by = Some(id);
                        stands = anew;
                    }
                    Err(Error::OutcomeUnknown) => return Err(Error::OutcomeUnknown),
                    Err(error) => unread.kept_because = Some(error.to_string()),
                }
            }
            older.push(stands);
            scrubbed.unread.push(unread);
        }
        Ok(scrubbed)
    }

    /// Puts the pack of `segment`, indexed anew behind `before`, in the place of
    /// the live segment `part`. The id of the new segment, and the segment.
    fn index_anew(
        &mut self,
        repository: &str,
        part: &(String, bool),
        segment: &Arc<Segment>,
        before: &SegmentSet,
    ) -> Result<(String, Arc<Segment>)> {
        let content = alt_pack_segment::indexed_anew(segment, before)
            .map_err(|_| Error::Integrity("pack of the segment does not verify"))?;
        let (id, path) = self.write_segment(repository, &content)?;
        let inline = content.len() <= INLINE_SEGMENT_BYTES;
        let anew = Segment::open(&path).map(Arc::new);
        let whole = anew.as_ref().is_ok_and(|anew| {
            alt_pack_segment::unread(anew, before).is_ok_and(|lost| lost.is_empty())
        });
        if !whole {
            let _ = std::fs::remove_file(&path);
            return Err(Error::Integrity("pack segment indexed anew is not read"));
        }
        let parts = std::slice::from_ref(part);
        match self.publish_merge(repository, parts, &id, &content, &path, inline) {
            Ok(Some(segment)) => {
                self.adopt_segment(repository, &id, Arc::clone(&segment))?;
                Ok((id, segment))
            }
            // the outcome is not known: a sweep removes the file if the row is not there
            Err(Error::OutcomeUnknown) => Err(Error::OutcomeUnknown),
            other => {
                let _ = std::fs::remove_file(&path);
                other?;
                Err(Error::Precondition("pack segment is no longer live"))
            }
        }
    }
}
