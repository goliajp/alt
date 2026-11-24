//! Removing what merges left behind, and files no row knows.

use crate::{
    Result, Store,
    store::{instance, instance_unlocked, integer, now},
};
use alt_remote_protocol::DeploymentMode;
use std::{collections::HashSet, path::PathBuf, time::Duration};

/// A file without a row younger than this may belong to a publication in flight,
/// which writes its file before its transaction.
const UNKNOWN_FILE_AGE: Duration = Duration::from_secs(3600);

impl Store {
    /// Deletes the segments of `repository` that were retired at least `grace_ms` ago,
    /// and files in its directory that no row knows. Returns how many files went.
    pub fn sweep_packs(&mut self, repository: &str, grace_ms: u64) -> Result<usize> {
        if let Some(known) = self.segments_unless_expired(repository, grace_ms)?
            && self.unknown_files(repository, &known)?.is_empty()
        {
            return Ok(0);
        }
        let (expired, known) = self.expire_segments(repository, grace_ms)?;
        let mut removed = 0;
        for id in &expired {
            let path = self.pack_path(repository, id)?;
            crate::open_segments::forget(&path);
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        for path in self.unknown_files(repository, &known)? {
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// The files in the directory of `repository` that no row in `known` names and
    /// that are too old to belong to a transaction still writing its row: a crash
    /// between writing a file and committing its row leaves the file.
    fn unknown_files(&self, repository: &str, known: &HashSet<String>) -> Result<Vec<PathBuf>> {
        let mut unknown = Vec::new();
        for entry in std::fs::read_dir(self.pack_directory(repository)?)? {
            let entry = entry?;
            let path = entry.path();
            let named = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| known.contains(stem));
            let old = entry
                .metadata()?
                .modified()?
                .elapsed()
                .is_ok_and(|age| age >= UNKNOWN_FILE_AGE);
            if !named && old {
                unknown.push(path);
            }
        }
        Ok(unknown)
    }

    /// The segments of `repository` that have a row, read without a lock; None when
    /// the time of a retired one has run out. This is every sweep but a few, and it
    /// follows every publication: a row lock here would give the transaction
    /// something to flush and hold back the next push to the repository meanwhile.
    /// Nothing is deleted on the strength of this read: a file it does not know may
    /// have a row that is not committed yet.
    fn segments_unless_expired(
        &mut self,
        repository: &str,
        grace_ms: u64,
    ) -> Result<Option<HashSet<String>>> {
        if self.spec.deployment_mode == DeploymentMode::Cloud {
            return Ok(None);
        }
        let mut tx = self.client.transaction()?;
        instance_unlocked(&mut tx, &self.spec, &self.signing_key)?;
        let time = now(&mut tx)?;
        let rows = tx.query(
            "SELECT segment_id,(state='retired' AND retired_at<=$2) IS TRUE FROM alt_pack_segments WHERE repository=$1",
            &[&repository, &integer(time.saturating_sub(grace_ms))?],
        )?;
        tx.commit()?;
        if rows.iter().any(|row| row.get(1)) {
            return Ok(None);
        }
        Ok(Some(rows.iter().map(|row| row.get(0)).collect()))
    }

    /// Deletes the rows of the retired segments whose time has run out. Returns
    /// their ids and the segments that still have a row.
    fn expire_segments(
        &mut self,
        repository: &str,
        grace_ms: u64,
    ) -> Result<(Vec<String>, HashSet<String>)> {
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
        Ok((expired, known))
    }
}
