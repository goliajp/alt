//! `alt fsck`: the full check of everything a repository stores.
//!
//! Commands verify what they read and trust the rest: a lookup goes through
//! an index straight to one record, and an open starts from the ref
//! snapshot instead of the first operation. Damage in a record that no
//! command happens to read stays unseen until this reads all of them.
//!
//! It also says what a command passes over without a word: bytes at the
//! end of a file that an interrupted append left, and operations and
//! records that were never synced and name what is not stored. Those a
//! command that writes cuts off; they are not damage. Where what is cut
//! holds whole records, the check says how much goes, which command takes
//! it, and does not pass ([`tails`]). It changes no file.

mod tails;

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::native::Store;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// The append files below the object store's three maps, as
/// [`alt_odb::OdbChecked::unconfirmed`] counts them.
const MAPS: [&str; 3] = ["store/manifests/blobmap", "tier1", "map.alt"];

/// An append file past the records held of it, and the byte from which on
/// its records are not known to have been synced, where that is recorded.
type End = (PathBuf, alt_odb::Passed, Option<u64>);

/// How the five append files go on past the records held of them: the
/// active pack, the blob map, the Tier 1 map, `map.alt`, the operation
/// log.
fn ends(store: &mut Store) -> Res<Vec<End>> {
    let passed = store.odb().passed().map_err(with_cause)?;
    let bounds = store.odb().unsynced_from().map_err(with_cause)?;
    let mut ends: Vec<End> = passed
        .into_iter()
        .zip(bounds)
        .map(|((path, passed), bound)| (path, passed, bound))
        .collect();
    let path = store.alt_dir().join("oplog/log");
    let log = store.refs_mut().oplog();
    let (holds, len, whole_at) = log.passed().map_err(with_cause)?;
    let passed = alt_odb::Passed {
        holds,
        len,
        whole_at,
    };
    ends.push((path, passed, log.unsynced_from()));
    Ok(ends)
}

/// The entries of every workspace's index that name a blob which is not
/// stored, or one that goes with the records a writer cuts off: an index
/// is written with its own fsync and can be ahead of the object store.
fn unstored_index_entries(store: &Store) -> Res<Vec<String>> {
    let alt_dir = store.alt_dir();
    let mut indexes = vec![(String::new(), alt_dir.join("index"))];
    if let Ok(listing) = std::fs::read_dir(alt_dir.join("workspaces")) {
        let mut named: Vec<_> = listing.filter_map(|entry| entry.ok()).collect();
        named.sort_by_key(|entry| entry.file_name());
        for entry in named {
            let of = format!(" of workspace {}", entry.file_name().to_string_lossy());
            indexes.push((of, entry.path().join("index")));
        }
    }
    let gone = store.odb().unconfirmed_objects().map_err(with_cause)?;
    let mut unstored = Vec::new();
    for (of, path) in indexes {
        let index = match alt_git_index::Index::open(&path, store.algo()?) {
            Err(alt_git_index::IndexError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            index => index?,
        };
        for entry in index.entries {
            // a gitlink names a commit of another repository
            if entry.mode & 0o170000 == 0o160000 {
                continue;
            }
            let stored = !gone.contains(&entry.oid)
                && store.odb().contains(&entry.oid).map_err(with_cause)?;
            if !stored {
                unstored.push(format!(
                    "index entry {}{of} names blob {}, which is not stored: add the file again",
                    entry.path, entry.oid
                ));
            }
        }
    }
    Ok(unstored)
}

pub fn run<W: Write>(store: &mut Store, json: bool, out: &mut W) -> Res<u8> {
    // looked for before the store is opened, which may replace them
    let ignored = store.odb().unusable_index_runs().map_err(with_cause)?;
    // a file that ends short of the records its marker says were synced
    store.odb().check_synced().map_err(with_cause)?;
    store
        .refs_mut()
        .oplog()
        .check_synced()
        .map_err(with_cause)?;
    let ends = ends(store)?;
    let odb = store.odb().check().map_err(with_cause)?;
    let refs = store.refs_mut().check().map_err(with_cause)?;
    let unconfirmed = store.unconfirmed()?;
    let named = unconfirmed.map_or(0, |found| found.ops);
    let dangling = store.dangling_refs(named)?;
    let unstored = unstored_index_entries(store)?;
    let found = Found {
        ends: &ends,
        odb: &odb,
        unconfirmed: named,
        dangling: &dangling,
        settles: store.refs_mut().unsettled(),
    };
    let (mut notes, mut damage, tails) = findings(store.alt_dir(), &found);
    // an index ahead of the store is what a power cut leaves beside
    // records that are about to be cut; on its own it is reported
    match tails && damage.is_empty() {
        true => notes.extend(unstored),
        false => damage.extend(unstored),
    }
    if !damage.is_empty() {
        return Err(format!("fsck: {}", damage.join("\nfsck: ")).into());
    }
    let discards = match tails {
        true => Some(tails::discards(store, &found, unconfirmed)?),
        false => None,
    };
    if let Some(tail) = &discards {
        notes.extend(tails::told(tail));
    }
    let counts = [
        ("chunk_records", "chunk records", odb.chunk_records),
        ("blob_records", "blob records", odb.blob_records),
        ("objects", "objects", odb.objects),
        ("decomposed_blobs", "decomposed blobs", odb.decomposed),
        ("operations", "operations", refs.ops),
        ("refs", "refs", refs.refs),
    ];
    report(out, json, &ignored, &notes, &counts, discards.as_ref())?;
    match discards {
        Some(tail) => Err(tails::failure(&tail).into()),
        None => Ok(0),
    }
}

/// Writes what the check found that is not damage, and what it verified.
fn report<W: Write>(
    out: &mut W,
    json: bool,
    ignored: &[PathBuf],
    notes: &[String],
    counts: &[(&'static str, &str, u64)],
    discards: Option<&tails::Tail>,
) -> Res<()> {
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
        if let Some(discards) = discards {
            fields.push(("unconfirmed_tail", tails::json(discards)));
        }
        json::emit(out, fields)?;
        return Ok(());
    }
    // an index is a cache: without the run its log is read instead
    for path in ignored {
        writeln!(
            out,
            "ignored index run {}: not a complete run",
            path.display()
        )?;
    }
    for note in notes {
        writeln!(out, "{note}")?;
    }
    for (_, label, count) in counts {
        writeln!(out, "{count:>12} {label}")?;
    }
    match discards {
        Some(_) => writeln!(out, "every record before the unconfirmed tail verified")?,
        None => writeln!(out, "every record verified")?,
    }
    Ok(())
}

/// What the check read that `findings` tells of.
struct Found<'a> {
    ends: &'a [End],
    odb: &'a alt_odb::OdbChecked,
    /// operations that were never synced and name objects not stored
    unconfirmed: usize,
    dangling: &'a [(String, alt_git_codec::ObjectId)],
    /// whether a command that writes settles the object store whatever
    /// it writes ([`tails::cutter`])
    settles: bool,
}

/// What the check has to tell that is not damage, what is, and whether a
/// command that writes takes whole records away: nothing past a boundary
/// that holds was ever confirmed, so a writer cuts off a record there
/// that does not read with the whole ones after it, and the records and
/// operations that name what is not stored.
fn findings(alt_dir: &Path, found: &Found) -> (Vec<String>, Vec<String>, bool) {
    let name = |path: &Path| {
        let inside = path.strip_prefix(alt_dir).unwrap_or(path);
        inside.display().to_string()
    };
    let (mut notes, mut damage, mut tails) = (Vec::new(), Vec::new(), false);
    for (path, passed, bound) in found.ends {
        let (file, holds) = (name(path), passed.holds);
        let bytes = passed.len.saturating_sub(holds);
        match passed.whole_at {
            None if bytes > 0 => notes.push(format!(
                "passed over {bytes} byte(s) at the end of {file}: an append that never \
                 finished; {} cuts them off",
                tails::cutter(&file, found.settles)
            )),
            None => {}
            Some(at) if bound.is_some_and(|bound| holds >= bound) => {
                tails = true;
                notes.push(tails::found(&file, holds, bytes, at));
            }
            Some(at) => damage.push(format!(
                "{file} reads up to byte {holds}, and a whole record begins past that, at byte {at}"
            )),
        }
    }
    for (file, bytes) in MAPS.iter().zip(found.odb.unconfirmed) {
        if bytes > 0 {
            tails = true;
            notes.push(format!(
                "{bytes} byte(s) of records at the end of {file} were never synced and name \
                 content that is not stored: {} cuts them off",
                tails::cutter(file, found.settles)
            ));
        }
    }
    if found.unconfirmed > 0 {
        tails = true;
        notes.push(format!(
            "{} operation(s) at the end of the operation log were never synced and name \
             objects that are not stored: the next command that writes discards them",
            found.unconfirmed
        ));
    }
    for (name, oid) in found.dangling {
        damage.push(format!("ref {name} points at {oid}, which is not stored"));
    }
    (notes, damage, tails)
}

fn with_cause(error: impl std::error::Error + 'static) -> Box<dyn std::error::Error> {
    format!("fsck: {}", crate::machine::causes(&error)).into()
}
