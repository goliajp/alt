//! Merging a repository's pack segments, and removing what merges left behind.
//!
//! Every push adds a segment and every lookup tries them one after another, so the
//! newest segments are merged whenever enough of similar size have gathered: sizes
//! then grow in tiers and the number of live segments with the logarithm of the
//! pushes. A merge changes no content. Readers that started before it keep the files
//! they opened; the merged-away segments stay on disk for a while for those that
//! were between reading the list and opening the files.

use crate::{
    Error, Result, Store,
    packs::{INLINE_SEGMENT_BYTES, NewSegment, forget_segment, insert_segment},
    store::{instance, integer, now, unsigned},
};
use alt_pack_segment::{Segment, SegmentSet};
use std::{collections::HashSet, sync::Arc, time::Duration};

/// fewest segments a merge is worth
const RUN: usize = 8;
/// a merge is assembled in memory
const MERGE_BYTES: u64 = 1 << 30;
/// How long a retired segment's file should outlive its retirement: far longer than a
/// reader takes from listing the live segments to opening their files.
pub const PACK_RETIRED_GRACE_MS: u64 = 600_000;
/// A file without a row younger than this may belong to a publication in flight,
/// which writes its file before its transaction.
const UNKNOWN_FILE_AGE: Duration = Duration::from_secs(3600);

/// Where the run to merge starts: the longest stretch of the newest segments whose
/// oldest member is no more than twice as large as all the newer ones together.
/// What stays out then at least doubles from each segment to the next older one, so
/// the segments a repository keeps grow with the logarithm of its size.
fn run_start(bytes: &[u64]) -> Option<usize> {
    let mut start = bytes.len();
    let mut total = 0u64;
    for (at, &next) in bytes.iter().enumerate().rev() {
        if total + next > MERGE_BYTES {
            break;
        }
        // a larger segment does not end the search: older small ones still fit under
        // the sum that includes it
        if at + 1 == bytes.len() || next <= total.saturating_mul(2) {
            start = at;
        }
        total += next;
    }
    (bytes.len() - start >= RUN).then_some(start)
}

impl Store {
    /// Merges the newest live segments of `repository` into one if enough have
    /// gathered. True when a merge was published; the caller asks again, because the
    /// merged segment can complete the next tier.
    pub fn compact_packs(&mut self, repository: &str) -> Result<bool> {
        let mut tx = self.client.transaction()?;
        let rows = tx.query(
            "SELECT segment_id,inline IS NOT NULL,bytes FROM alt_pack_segments WHERE repository=$1 AND state='live' ORDER BY sequence,segment_id",
            &[&repository],
        )?;
        tx.commit()?;
        let mut ids = Vec::with_capacity(rows.len());
        let mut bytes = Vec::with_capacity(rows.len());
        for row in &rows {
            ids.push((row.get::<_, String>(0), row.get::<_, bool>(1)));
            bytes.push(unsigned(row.get(2))?);
        }
        let Some(start) = run_start(&bytes) else {
            return Ok(false);
        };
        let set = self.open_segments(repository, &ids)?;
        let (older, parts) = set.segments().split_at(start);
        let content = alt_pack_segment::merge(parts, &SegmentSet::new(older.to_vec()))
            .map_err(|_| Error::Integrity("pack segments do not merge"))?;
        let mut nonce = [0u8; 16];
        alt_os::random::fill(&mut nonce);
        let id: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = self.pack_path(repository, &id)?;
        let inline = content.len() <= INLINE_SEGMENT_BYTES;
        {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(&content)?;
            if !inline {
                file.sync_all()?;
                std::fs::File::open(path.parent().expect("segment directory"))?.sync_all()?;
            }
        }
        let published = self.publish_merge(repository, &ids[start..], &id, &content, &path, inline);
        match published {
            Ok(Some(segment)) => {
                self.adopt_segment(repository, &id, segment)?;
                Ok(true)
            }
            Ok(None) => {
                let _ = std::fs::remove_file(&path);
                Ok(false)
            }
            // the merge may have committed: the sweep removes the file if it did not
            Err(Error::OutcomeUnknown) => Err(Error::OutcomeUnknown),
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                Err(error)
            }
        }
    }

    /// Replaces the rows of `parts` with the row of the merged segment. None when a
    /// part is no longer live: another merge got there first.
    fn publish_merge(
        &mut self,
        repository: &str,
        parts: &[(String, bool)],
        id: &str,
        content: &[u8],
        path: &std::path::Path,
        inline: bool,
    ) -> Result<Option<Arc<Segment>>> {
        let segment =
            Arc::new(Segment::open(path).map_err(|_| Error::Integrity("merged pack segment"))?);
        let part_ids: Vec<&str> = parts.iter().map(|(id, _)| id.as_str()).collect();
        let mut tx = self.client.transaction()?;
        // a backup holds the instance row while it copies the files of the live
        // segments; like every write, a merge waits behind it
        instance(&mut tx, &self.spec, &self.signing_key)?;
        // publications take this lock too, so the set of live segments stands still
        tx.query_one(
            "SELECT 1 FROM alt_repositories WHERE id=$1 FOR UPDATE",
            &[&repository],
        )?;
        let time = now(&mut tx)?;
        let retired = tx.query(
            "UPDATE alt_pack_segments SET state='retired',retired_at=$3 WHERE repository=$1 AND segment_id=ANY($2) AND state='live' RETURNING sequence,bytes",
            &[&repository, &part_ids, &integer(time)?],
        )?;
        if retired.len() != parts.len() {
            return Ok(None);
        }
        let mut sequence = 0;
        let mut before = 0i64;
        for row in &retired {
            sequence = sequence.max(unsigned(row.get(0))?);
            before += row.get::<_, i64>(1);
        }
        insert_segment(
            &mut tx,
            repository,
            sequence,
            &NewSegment {
                id,
                objects: segment.len() as u64,
                bytes: content.len() as u64,
                flags: segment.flags(),
                inline: inline.then_some(content),
            },
        )?;
        tx.execute(
            "UPDATE alt_repositories SET stored_bytes=greatest(stored_bytes+$2,0) WHERE id=$1",
            &[&repository, &(integer(content.len() as u64)? - before)],
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(Some(segment))
    }

    /// Deletes the segments of `repository` that were retired at least `grace_ms` ago,
    /// and files in its directory that no row knows. Returns how many files went.
    pub fn sweep_packs(&mut self, repository: &str, grace_ms: u64) -> Result<usize> {
        let mut tx = self.client.transaction()?;
        // not while a backup is copying files that were live when it began
        instance(&mut tx, &self.spec, &self.signing_key)?;
        tx.query_one(
            "SELECT 1 FROM alt_repositories WHERE id=$1 FOR UPDATE",
            &[&repository],
        )?;
        let time = now(&mut tx)?;
        let expired: Vec<String> = tx
            .query(
                "DELETE FROM alt_pack_segments WHERE repository=$1 AND state='retired' AND retired_at<=$2 RETURNING segment_id",
                &[&repository, &integer(time.saturating_sub(grace_ms))?],
            )?
            .iter()
            .map(|row| row.get(0))
            .collect();
        let known: HashSet<String> = tx
            .query(
                "SELECT segment_id FROM alt_pack_segments WHERE repository=$1",
                &[&repository],
            )?
            .iter()
            .map(|row| row.get(0))
            .collect();
        tx.commit()?;
        let mut removed = 0;
        for id in &expired {
            let path = self.pack_path(repository, id)?;
            forget_segment(&path);
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        // a crash between writing a file and committing its row leaves the file
        for entry in std::fs::read_dir(self.pack_directory(repository)?)? {
            let entry = entry?;
            let path = entry.path();
            let stem = path.file_stem().and_then(|stem| stem.to_str());
            let unknown = stem
                .is_some_and(|stem| !known.contains(stem) && !expired.iter().any(|id| id == stem));
            let old = entry
                .metadata()?
                .modified()?
                .elapsed()
                .is_ok_and(|age| age >= UNKNOWN_FILE_AGE);
            if unknown && old && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::run_start;

    #[test]
    fn a_run_needs_enough_segments_of_similar_size() {
        assert_eq!(run_start(&[]), None);
        assert_eq!(run_start(&[10; 7]), None);
        assert_eq!(run_start(&[10; 8]), Some(0));
        // the old large segment stays out until the newer ones weigh half of it
        assert_eq!(run_start(&[1000, 10, 10, 10, 10, 10, 10, 10, 10]), Some(1));
        assert_eq!(run_start(&[150, 10, 10, 10, 10, 10, 10, 10, 10]), Some(0));
        // a small segment under larger newer ones joins them
        assert_eq!(run_start(&[1, 10, 10, 10, 10, 10, 10, 10, 10]), Some(0));
        // seven are not a run, however the eighth compares
        assert_eq!(run_start(&[1000, 10, 10, 10, 10, 10, 10, 10]), None);
        // a small newest segment does not hide the run behind a larger one
        assert_eq!(run_start(&[1000, 4, 4, 4, 4, 4, 4, 4, 90, 3]), Some(1));
        assert_eq!(
            run_start(&[
                18166956, 37852, 28907, 51359, 10622, 321048, 31993, 406481, 940165, 1106760,
                571621, 1785647, 70351
            ]),
            Some(1)
        );
        // each segment more than twice all the newer ones: nothing to merge
        assert_eq!(
            run_start(&[19683, 6561, 2187, 729, 243, 81, 27, 9, 3, 1]),
            None
        );
        assert_eq!(
            run_start(&[1 << 30, 10, 10, 10, 10, 10, 10, 10, 10]),
            Some(1)
        );
    }
}
