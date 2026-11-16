//! `alt fsck`: the full check of everything a repository stores.
//!
//! Commands verify what they read and trust the rest: a lookup goes through
//! an index straight to one record, and an open starts from the ref
//! snapshot instead of the first operation. Damage in a record that no
//! command happens to read stays unseen until this reads all of them.

use std::io::Write;

use crate::json::{self, Json};
use crate::native::Store;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

pub fn run<W: Write>(store: &mut Store, json: bool, out: &mut W) -> Res<u8> {
    // looked for before the store is opened, which may replace them
    let ignored = store.odb().unusable_index_runs().map_err(with_cause)?;
    let odb = store.odb().check().map_err(with_cause)?;
    let refs = store.refs_mut().check().map_err(with_cause)?;
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
        for (_, label, count) in counts {
            writeln!(out, "{count:>12} {label}")?;
        }
        writeln!(out, "every record verified")?;
    }
    Ok(0)
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
