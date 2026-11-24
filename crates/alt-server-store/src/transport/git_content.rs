use crate::{Credential, Error, Result, Store};
use alt_remote_protocol::Scope;

impl Store {
    /// A published Git object by the digest of its content. Git objects live in the
    /// pack segments, whichever protocol brought them in.
    ///
    /// The segments of a repository whose history was replaced, or that had a
    /// branch deleted, hold objects no ref leads to. With `reached`, such a
    /// repository gives by digest what its refs reach and nothing else; in any
    /// other, every object of the segments is reached.
    pub(super) fn open_published_git_content(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
        reached: bool,
    ) -> Result<(u64, crate::StoredReader)> {
        let view = self.pack_view(credential, &scope.repository_instance_id)?;
        if &view.scope != scope {
            return Err(Error::Unauthorized);
        }
        if reached && view.unreachable {
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
