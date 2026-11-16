//! `alt fsck`: the full check of everything a repository stores.
//!
//! Commands verify what they read and trust the rest: a lookup goes through
//! an index straight to one record, and an open starts from the ref
//! snapshot instead of the first operation. Damage in a record that no
//! command happens to read stays unseen until this reads all of them.
//!
//! It also says what a command passes over without a word: bytes at the
//! end of a file that an interrupted append left, and operations and
//! records that were never synced and name what is not stored. Those the
//! next command that writes cuts off; they are not damage. It changes no
//! file.

use std::io::Write;
use std::path::Path;

use crate::json::{self, Json};
use crate::native::Store;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// The append files below the object store's three maps, as
/// [`alt_odb::OdbChecked::unconfirmed`] counts them.
const MAPS: [&str; 3] = ["store/manifests/blobmap", "tier1", "map.alt"];

pub fn run<W: Write>(store: &mut Store, json: bool, out: &mut W) -> Res<u8> {
    // looked for before the store is opened, which may replace them
    let ignored = store.odb().unusable_index_runs().map_err(with_cause)?;
    // a file that ends short of the records its marker says were synced
    store.odb().check_synced().map_err(with_cause)?;
    let log = store.refs_mut().oplog();
    log.check_synced().map_err(with_cause)?;
    let (holds, len, whole_at) = log.passed().map_err(with_cause)?;
    let mut passed = store.odb().passed().map_err(with_cause)?;
    passed.push((
        store.alt_dir().join("oplog/log"),
        alt_odb::Passed {
            holds,
            len,
            whole_at,
        },
    ));
    let odb = store.odb().check().map_err(with_cause)?;
    let refs = store.refs_mut().check().map_err(with_cause)?;
    let unconfirmed = store.unconfirmed()?.map_or(0, |found| found.ops);
    let dangling = store.dangling_refs(unconfirmed)?;

    let (notes, damage) = findings(store.alt_dir(), &passed, &odb, unconfirmed, &dangling);
    if !damage.is_empty() {
        return Err(format!("fsck: {}", damage.join("\nfsck: ")).into());
    }

    let counts = [
        ("chunk_records", "chunk records", odb.chunk_records),
        ("blob_records", "blob records", odb.blob_records),
        ("objects", "objects", odb.objects),
        ("decomposed_blobs", "decomposed blobs", odb.decomposed),
        ("operations", "operations", refs.ops),
        ("refs", "refs", refs.refs),
    ];
    if json {
        let mut fields: Vec<_> = counts
            .iter()
            .map(|(key, _, count)| (*key, Json::Num(*count as i64)))
            .collect();
        let paths = ignored
            .iter()
            .map(|path| Json::str(path.to_string_lossy().as_bytes()));
        fields.push(("ignored_index_runs", Json::Array(paths.collect())));
        let notes = notes.iter().map(|note| Json::str(note.as_bytes()));
        fields.push(("notes", Json::Array(notes.collect())));
        json::emit(out, fields)?;
    } else {
        // an index is a cache: without the run its log is read instead
        for path in &ignored {
            writeln!(
                out,
                "ignored index run {}: not a complete run",
                path.display()
            )?;
        }
        for note in &notes {
            writeln!(out, "{note}")?;
        }
        for (_, label, count) in counts {
            writeln!(out, "{count:>12} {label}")?;
        }
        writeln!(out, "every record verified")?;
    }
    Ok(0)
}

/// What the check has to tell that is not damage, and what is.
fn findings(
    alt_dir: &Path,
    passed: &[(std::path::PathBuf, alt_odb::Passed)],
    odb: &alt_odb::OdbChecked,
    unconfirmed: usize,
    dangling: &[(String, alt_git_codec::ObjectId)],
) -> (Vec<String>, Vec<String>) {
    let name = |path: &Path| {
        let inside = path.strip_prefix(alt_dir).unwrap_or(path);
        inside.display().to_string()
    };
    let mut notes = Vec::new();
    let mut damage = Vec::new();
    for (path, passed) in passed {
        let file = name(path);
        if passed.len > passed.holds {
            let bytes = passed.len - passed.holds;
            notes.push(format!(
                "passed over {bytes} byte(s) at the end of {file}: an append that never finished"
            ));
        }
        if let Some(at) = passed.whole_at {
            let holds = passed.holds;
            damage.push(format!(
                "{file} reads up to byte {holds}, and a whole record begins past that, at byte {at}"
            ));
        }
    }
    for (file, bytes) in MAPS.iter().zip(odb.unconfirmed) {
        if bytes > 0 {
            notes.push(format!(
                "{bytes} byte(s) of records at the end of {file} were never synced and name \
                 content that is not stored: the next command that writes cuts them off"
            ));
        }
    }
    if unconfirmed > 0 {
        notes.push(format!(
            "{unconfirmed} operation(s) at the end of the operation log were never synced and \
             name objects that are not stored: the next command that writes discards them"
        ));
    }
    for (name, oid) in dangling {
        damage.push(format!("ref {name} points at {oid}, which is not stored"));
    }
    (notes, damage)
}

/// Spells out the whole cause chain: the outer errors of the storage layers
/// only name the layer.
fn with_cause(error: impl std::error::Error) -> Box<dyn std::error::Error> {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    format!("fsck: {text}").into()
}
