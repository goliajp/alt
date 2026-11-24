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

const NAMED: &str = "SELECT EXISTS(SELECT 1 FROM alt_task_bundles b JOIN alt_requests q ON q.repository=b.repository AND q.epoch=b.epoch AND q.actor_id=b.actor_id AND q.request_id=b.request_id WHERE b.repository=$1 AND q.state='committed' AND q.manifest->'entries' @> jsonb_build_array(jsonb_build_object('blake3',$2::text))) OR EXISTS(SELECT 1 FROM alt_archived_publications p WHERE p.repository=$1 AND EXISTS(SELECT 1 FROM alt_archived_records r WHERE r.repository=p.repository AND r.digest=p.digest) AND p.manifest->'entries' @> jsonb_build_array(jsonb_build_object('blake3',$2::text)))";

impl Store {
    /// A published Git object by the digest of its content. Git objects live in the
    /// pack segments, whichever protocol brought them in.
    ///
    /// The segments of a repository whose history was replaced, or that had a
    /// branch deleted, hold objects no ref leads to. Such a repository gives by
    /// digest what `led` says and nothing else; in any other, every object of
    /// the segments is reached.
    pub(super) fn open_published_git_content(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
        led: Led,
    ) -> Result<(u64, crate::StoredReader)> {
        let view = self.pack_view(credential, &scope.repository_instance_id)?;
        if &view.scope != scope {
            return Err(Error::Unauthorized);
        }
        if view.unreachable && !(led == Led::RefsOrRecords && self.named(scope, digest)?) {
            let key = alt_provenance::decode_hex::<32>(digest).map_err(|_| Error::Unauthorized)?;
            let found = view.segments.open_blake3(&key);
            let found = found.map_err(|_| Error::Integrity("published object bytes"))?;
            let Some((oid, ..)) = found else {
                return Err(Error::Unauthorized);
            };
            let id = |oid: &alt_git_codec::ObjectId| -> alt_pack_segment::Oid {
                oid.as_bytes().try_into().expect("sha1 object id")
            };
            let tips: Vec<_> = view.refs.values().map(|oid| id(&oid)).collect();
            let steps = self.limits.walk_steps;
            let reached = view.segments.select_while(&tips, &[], steps, None);
            let reached = reached.map_err(|error| match error {
                alt_pack_segment::Error::Limit(_) => Error::Limit("objects the refs reach"),
                _ => Error::Integrity("objects the refs reach"),
            })?;
            if !reached.objects().contains(&oid) {
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
        let row = tx.query_one(NAMED, &[&scope.repository_instance_id, &digest])?;
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
