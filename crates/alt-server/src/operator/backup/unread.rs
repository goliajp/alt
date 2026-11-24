//! Segments that do not give back every object of their index, as a backup and
//! a restoration are held to them.
//!
//! A backup holds the files of the segments to their digests, and a file whose
//! index leads to no object, or to another, has the digest it was copied with.
//! So every object is read before an instance is backed up and before a
//! restored one is activated. Where some are not read, the operator is told
//! which, and mends them first. What cannot be mended does not keep the rest
//! from being backed up: the backup is made where that is asked for, names the
//! segments in its manifest, and a restoration of it tells of them again.

use super::{Error, Result};
use alt_json::{Value, json};

/// A segment of a backup that does not give back all of its objects.
#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Unread {
    pub repository: String,
    pub segment: String,
    pub objects: u64,
    /// the first of them, each with what its read ended in
    pub first: Vec<(String, String)>,
}

pub(super) const BACKED_UP: &str = "pack segments hold objects that are not read: mend them (scrub --mend) and back up again; where a segment stays unread, --with-unread backs the instance up as it is and names the segment in the manifest";
pub(super) const RESTORED: &str = "the restored pack segments hold objects that are not read, and the backup does not name them: nothing is left of this restoration, and it can be begun again; --with-unread restores the instance as it is, for scrub --mend to be run on it";

/// The segments of the instance that do not give back all of their objects,
/// read for `command`, which tells how far the reading has come.
pub(super) fn found(
    store: &mut alt_server_store::Store,
    command: &'static str,
) -> Result<Vec<Unread>> {
    let mut reading = crate::operator::reading::Reading::by(command);
    let mut read = |repository: &str, read, all| reading.read(repository, read, all);
    let unread = store.unread_packs(&mut read)?;
    let told = |(repository, unread): (String, alt_server_store::Unread)| Unread {
        repository,
        segment: unread.segment,
        objects: unread.objects,
        first: unread.named,
    };
    Ok(unread.into_iter().map(told).collect())
}

/// Whether a backup names each of `found` among the segments it was made with.
pub(super) fn told_of(found: &[Unread], named: &[Unread]) -> bool {
    let names = |unread: &Unread| {
        let same = |named: &&Unread| {
            named.repository == unread.repository && named.segment == unread.segment
        };
        named.iter().find(same).is_some()
    };
    found.iter().all(names)
}

/// What a backup or a restoration that is not made for them ends with.
pub(super) fn refused(what: &str, found: &[Unread], hint: &str) -> Error {
    let mut told = json!({"unread": found, "message": hint});
    told[what] = Value::Bool(false);
    Error::Partly(told, false)
}
