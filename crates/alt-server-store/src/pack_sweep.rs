//! Removing what merges left behind, and files no row knows.

use crate::{
    Result, Store,
    store::{instance, instance_unlocked, integer, now},
};
use alt_remote_protocol::DeploymentMode;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::Duration,
};

/// A file without a row younger than this may belong to a publication in flight,
/// which writes its file before its transaction.
const UNKNOWN_FILE_AGE: Duration = Duration::from_secs(3600);

/// the files of segments this process is writing and has no row for yet
static WRITTEN: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

/// The file of a segment from when it is made until its row is committed or
/// given up: no sweep of this process removes it meanwhile, however long the
/// pack was in coming and in being verified.
pub struct Writing(PathBuf);

/// Has the file at `path`, which is about to be made, kept from the sweep.
pub fn writing(path: &Path) -> Writing {
    WRITTEN.lock().unwrap().insert(path.to_owned());
    Writing(path.to_owned())
}

impl Drop for Writing {
    fn drop(&mut self) {
        WRITTEN.lock().unwrap().remove(&self.0);
    }
}

/// The files in `directory` that no row in `known` names, that this process is
/// not writing, and that are too old to belong to a transaction of another
/// process still writing its row: a crash between writing a file and
/// committing its row leaves the file.
fn unknown_files(directory: &Path, known: &HashSet<String>) -> Result<Vec<PathBuf>> {
    let mut unknown = Vec::new();
    for entry in std::fs::read_dir(directory)? {
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
        if !named && old && !WRITTEN.lock().unwrap().contains(&path) {
            unknown.push(path);
        }
    }
    Ok(unknown)
}

impl Store {
    /// Deletes the segments of `repository` that were retired at least `grace_ms` ago,
    /// and files in its directory that no row knows. Returns how many files went.
    pub fn sweep_packs(&mut self, repository: &str, grace_ms: u64) -> Result<usize> {
        let directory = self.pack_directory(repository)?;
        if let Some(known) = self.segments_unless_expired(repository, grace_ms)?
            && unknown_files(&directory, &known)?.is_empty()
        {
            return Ok(0);
        }
        let (expired, mut removed) = self.expire_segments(repository, grace_ms, &directory)?;
        for id in &expired {
            let path = self.pack_path(repository, id)?;
            crate::open_segments::forget(&path);
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
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

    /// Deletes the rows of the retired segments whose time has run out, and the
    /// files in `directory` that no row knows. Returns the ids of the first and
    /// how many of the second went.
    ///
    /// The files go while the row of the repository is locked. A publication
    /// looks for the file of its segment under the same lock: it either finds
    /// the file gone and publishes nothing, or has committed its row before the
    /// rows are read here. Removed after the lock was let go of, a file could
    /// lose its row's race and leave a live segment without its bytes.
    fn expire_segments(
        &mut self,
        repository: &str,
        grace_ms: u64,
        directory: &Path,
    ) -> Result<(Vec<String>, usize)> {
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
        let mut removed = 0;
        for path in unknown_files(directory, &known)? {
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        tx.commit()?;
        Ok((expired, removed))
    }
}
