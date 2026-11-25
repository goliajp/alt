use crate::{Credential, Error, Result, Store, reading};
use alt_remote_protocol::Scope;

/// Whom the segments of a repository give a Git object to by the digest of its
/// content, once its history was replaced or a branch of it deleted and they
/// hold objects no ref leads to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Led {
    /// what the refs the caller reads lead to
    Refs,
    /// that, and what the manifest of a published publication of records names:
    /// one who receives the records is sent for each entry of it, whatever
    /// became of the ref it was published under
    RefsOrRecords,
}

// The manifests that name the content are found first, from their index, and
// whole: asked under the EXISTS, the planner goes through every request of the
// repository for the first that names it.
const NAMED: &str = "WITH q AS MATERIALIZED (SELECT epoch,actor_id,request_id FROM alt_requests WHERE repository=$1 AND state='committed' AND manifest->'entries' @> $2::text::jsonb), p AS MATERIALIZED (SELECT digest FROM alt_archived_publications WHERE repository=$1 AND manifest->'entries' @> $2::text::jsonb) SELECT EXISTS(SELECT 1 FROM q JOIN alt_task_bundles b ON b.repository=$1 AND b.epoch=q.epoch AND b.actor_id=q.actor_id AND b.request_id=q.request_id) OR EXISTS(SELECT 1 FROM p WHERE EXISTS(SELECT 1 FROM alt_archived_records r WHERE r.repository=$1 AND r.digest=p.digest))";

impl Store {
    /// A published Git object by the digest of its content. Git objects live in the
    /// pack segments, whichever protocol brought them in.
    ///
    /// The segments of a repository whose history was replaced, or that had a
    /// branch deleted, hold objects no ref leads to. Such a repository gives by
    /// digest what `led` says and nothing else; in any other, every object of
    /// the segments is reached. What the refs lead to is kept for the next
    /// that asks it of the same refs and segments, and is looked for no
    /// longer once the mark that comes with `led` is set.
    pub(super) fn open_published_git_content(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
        (led, gone): (Led, Option<&std::sync::atomic::AtomicBool>),
    ) -> Result<(u64, crate::StoredReader)> {
        let view = self.pack_view(credential, &scope.repository_instance_id)?;
        if &view.scope != scope {
            return Err(Error::Unauthorized);
        }
        if view.unreachable {
            let key = alt_provenance::decode_hex::<32>(digest).map_err(|_| Error::Unauthorized)?;
            let found = view.segments.open_blake3(&key);
            let found = found.map_err(|_| Error::Integrity("published object bytes"))?;
            let Some((oid, ..)) = found else {
                return Err(Error::Unauthorized);
            };
            // the records are asked last: what the refs lead to is kept, and every
            // manifest is read for what they name
            if !self.led(&view, gone)?.to(&oid)
                && !(led == Led::RefsOrRecords && self.named(scope, digest)?)
            {
                return Err(Error::Unauthorized);
            }
        }
        segment_content(&view.segments, digest)?.ok_or(Error::Unauthorized)
    }
    /// Whether a publication of records that is published names the content in
    /// its manifest. The manifests are read one by one: this is asked of a
    /// repository that holds what no ref leads to, for a Git object alone.
    fn named(&mut self, scope: &Scope, digest: &str) -> Result<bool> {
        let mut tx = reading::transaction(&mut self.client, &self.spec)?;
        // a constant the index of the manifests is searched with; the digest is hex
        let entry = format!("[{{\"blake3\":\"{digest}\"}}]");
        let row = tx.query_one(NAMED, &[&scope.repository_instance_id, &entry])?;
        tx.commit()?;
        Ok(row.get(0))
    }
}
/// The object whose content has this digest, inflated into an unnamed temporary file
/// as it is read from its segment.
pub(crate) fn segment_content(
    segments: &alt_pack_segment::SegmentSet,
    digest: &str,
) -> Result<Option<(u64, crate::StoredReader)>> {
    let key = alt_provenance::decode_hex::<32>(digest).map_err(|_| Error::Unauthorized)?;
    let Some((_, _, size, mut stream)) = segments
        .open_blake3(&key)
        .map_err(|_| Error::Integrity("published object bytes"))?
    else {
        return Ok(None);
    };
    let mut file = alt_os::temp::tempfile()?;
    if std::io::copy(&mut stream, &mut file)? != size {
        return Err(Error::Integrity("published object bytes"));
    }
    Ok(Some((size, crate::StoredReader::from_file(file, size)?)))
}
