//! What a prepared index change names in the object store.
//!
//! The blobs of an index change are written before its intent is
//! appended to the operation log, and both are synced before the index
//! is replaced. After a power cut the kernel may have written the intent
//! out and not the blobs: such an intent lies past the boundary the
//! log's marker records, like a ref transaction whose objects never
//! arrived, and is cut and passed over by the same rule.

use std::sync::Arc;

use super::whole::FromFile;
use super::*;

/// The recovery snapshot `id` of the store under `alt_dir`.
pub(super) fn read_snapshot_at(alt_dir: &Path, id: &str) -> Res<Vec<u8>> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid recovery snapshot identity".into());
    }
    let bytes = std::fs::read(alt_dir.join("recovery/objects").join(id))?;
    if hash(&bytes) != id {
        return Err("corrupt recovery snapshot".into());
    }
    Ok(bytes)
}

impl NativeRepo<'_> {
    pub(in crate::native) fn snapshot(&self, bytes: &[u8]) -> Res<String> {
        let id = hash(bytes);
        let path = self.store.alt_dir.join("recovery/objects").join(&id);
        // byte for byte: what was there before, or what is written now
        let mut held = crate::durable::holds(&path, bytes)?;
        if held.is_none() {
            durable_write(&path, bytes)?;
            held = crate::durable::holds(&path, bytes)?;
        }
        match held {
            Some(true) => Ok(id),
            _ => Err("corrupt recovery snapshot".into()),
        }
    }

    fn recovery_object(&self, id: &str) -> PathBuf {
        self.store.alt_dir.join("recovery/objects").join(id)
    }

    /// [`Self::snapshot`] of bytes that were read from the file `read`,
    /// where they were read from one: a snapshot that is that very file
    /// holds them and is not read to be compared, and where the file is
    /// noted as that snapshot the bytes are not hashed either. An index
    /// change leaves the index and its snapshot as one file.
    pub(in crate::native) fn snapshot_as(
        &self,
        bytes: &[u8],
        read: Option<FromFile>,
    ) -> Res<String> {
        use std::os::unix::fs::MetadataExt;
        let Some(read) = read else {
            return self.snapshot(bytes);
        };
        let same = |meta: std::fs::Metadata| (meta.dev(), meta.ino()) == read.file;
        let is_the_file = |id: &str| std::fs::metadata(self.recovery_object(id)).is_ok_and(same);
        if let Some(id) = read.snapshot
            && is_the_file(&id)
        {
            return Ok(id);
        }
        let id = hash(bytes);
        if !is_the_file(&id) {
            self.keep_snapshot_as(&id, bytes)?;
        }
        Ok(id)
    }

    /// Keeps `bytes` as the snapshot `id`, their hash; whether this wrote
    /// the file, which is then as synced as what the thread writes is. A
    /// file that was there is compared byte for byte.
    pub(in crate::native) fn keep_snapshot_as(&self, id: &str, bytes: &[u8]) -> Res<bool> {
        let path = self.recovery_object(id);
        match crate::durable::holds(&path, bytes)? {
            Some(true) => Ok(false),
            Some(false) => Err("corrupt recovery snapshot".into()),
            None => {
                durable_write(&path, bytes)?;
                Ok(true)
            }
        }
    }

    /// Puts the snapshot `id`, which holds `bytes`, in place as the index
    /// of the workspace: the one file under both names, or a file of its
    /// own where the file system links none. `fresh` says this command
    /// wrote the snapshot.
    pub(in crate::native) fn put_index(&self, id: &str, bytes: &[u8], fresh: bool) -> Res<()> {
        let snapshot = self.recovery_object(id);
        if !crate::durable::link_state(&snapshot, &self.index_path, fresh)? {
            durable_write(&self.index_path, bytes)?;
        }
        self.note_whole(&std::fs::metadata(&self.index_path)?, Some(id));
        Ok(())
    }

    pub(in crate::native) fn read_snapshot(&self, id: &str) -> Res<Vec<u8>> {
        read_snapshot_at(&self.store.alt_dir, id)
    }
}

/// The blobs of the index that `op` prepares, where it is the intent of
/// an index change.
fn blobs(alt_dir: &Path, op: &alt_oplog::Op) -> Res<Vec<ObjectId>> {
    if op.payload.first() != Some(&PREPARE) {
        return Ok(Vec::new());
    }
    let intent: IndexIntent = decode(&op.payload, PREPARE)?;
    let index = read_snapshot_at(alt_dir, &intent.after)?;
    let algo = alt_repo::native_object_format(alt_dir)?;
    let entries = Index::parse(&index, algo)?.entries.into_iter();
    Ok(entries
        .filter(|entry| entry.mode & 0o170000 != 0o160000)
        .map(|entry| entry.oid)
        .collect())
}

/// For whoever looks for ops that were never synced and name what is not
/// stored: the blobs an index change of the store under `alt_dir` names.
pub fn prepared_blobs(alt_dir: &Path) -> alt_refs::AlsoNamed {
    let alt_dir = alt_dir.to_owned();
    // an intent or a snapshot that does not read names nothing here: the
    // recovery of that operation reports it
    Arc::new(move |op| blobs(&alt_dir, op).unwrap_or_default())
}

/// Writes the store under `alt_dir` out as a git repository in `target`,
/// without what was never synced and names what is not stored, an add
/// among it; a ref that reading past damage leaves out is told on stderr.
pub fn export_git(
    alt_dir: &Path,
    target: &Path,
) -> Result<alt_export::ExportReport, alt_export::ExportError> {
    let report = alt_export::export_git_naming(alt_dir, target, Some(&prepared_blobs(alt_dir)))?;
    for (name, oid) in &report.left_out {
        eprintln!("warning: left out ref {name}: it points at {oid}, which is not stored");
    }
    Ok(report)
}
