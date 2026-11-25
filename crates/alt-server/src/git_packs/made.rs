//! What a push publishes behind given segments: the segment of its pack,
//! written, and the publication of its updates.

use super::*;
use alt_server_store::HeldSegment;

/// The id and the file of a segment no file was written for yet.
pub(super) fn fresh(store: &Store, repository: &str) -> Result<(String, std::path::PathBuf)> {
    let mut nonce = [0u8; 16];
    alt_os::random::fill(&mut nonce);
    let id = hex(&nonce);
    let path = store.pack_path(repository, &id)?;
    Ok((id, path))
}

/// A push while it is published, and what is kept of it from one attempt to
/// the next.
pub(super) struct Push<'a> {
    pub(super) view: &'a PackView,
    pub(super) changes: &'a [&'a alt_wire::push::RefUpdate],
    pub(super) pack: &'a [u8],
    pub(super) room: Option<&'a crate::git::Verifying>,
    /// the file a pack came in, where it has one
    pub(super) spooled: Option<(&'a String, &'a std::path::PathBuf, &'a mut std::fs::File)>,
    /// the file made for it here, if any
    pub(super) created: &'a mut Option<std::path::PathBuf>,
    pub(super) limits: alt_remote_protocol::Limits,
    /// the pack as it was verified, once it was
    pub(super) verified: Option<alt_pack_segment::Ingested>,
    /// What the refs the one who pushes reads lead to, where the segments hold
    /// more than that. The pack and the updates refer to nothing beyond it
    /// that the pack does not carry: what a deleted branch held is not had
    /// back by its id.
    pub(super) led: Option<Arc<alt_pack_segment::Led>>,
}

/// What a push publishes behind given segments.
pub(super) struct Made {
    pub(super) key: RequestKey,
    pub(super) publication: Publication,
    /// the segments with the new one among them
    pub(super) set: SegmentSet,
    pub(super) stored: Option<(HeldSegment, Arc<Segment>)>,
    /// a file made here is kept from the sweep until its row is committed
    pub(super) _writing: Option<alt_server_store::Writing>,
}

impl Made {
    /// Once it is published: its segment is known to the requests after this one.
    pub(super) fn adopted(
        self,
        store: &Store,
        repository: &str,
        published: &mut bool,
    ) -> Result<Option<RequestKey>> {
        *published = self.stored.is_some();
        if let Some((stored, segment)) = self.stored {
            store.adopt_segment(repository, &stored.id, segment)?;
        }
        Ok(Some(self.key))
    }
}

impl Push<'_> {
    /// The updates of the push as a publication has them, each told whether it
    /// is forced, behind `set`.
    fn updates(&self, set: &SegmentSet) -> Result<Vec<RefUpdate>> {
        if let Some(led) = &self.led {
            let carried = |oid: &Oid| {
                self.verified
                    .as_ref()
                    .is_some_and(|pack| pack.contains(oid))
            };
            let news = self.changes.iter().filter_map(|update| update.new);
            if news.map(raw).any(|new| !carried(&new) && !led.to(&new)) {
                return Err(Error::Integrity("pack references an unavailable object"));
            }
        }
        let mut remaining = self.limits.walk_steps;
        let mut refs = Vec::new();
        for update in self.changes.iter().copied() {
            let force = match (update.old, update.new) {
                (Some(old), Some(new)) => {
                    update.name.starts_with("refs/tags/")
                        || !ancestor(set, &raw(old), &raw(new), &mut remaining)?
                }
                _ => false,
            };
            refs.push(RefUpdate {
                name: update.name.clone(),
                expected: update.old.map(|oid| oid.to_string()),
                new: update.new.map(|oid| oid.to_string()),
                force,
            });
        }
        Ok(refs)
    }

    /// The segment of the pack behind the segments `behind`, written, and the
    /// publication of the updates with it. The pack is verified the first time
    /// and told anew behind other segments after that: what its entries make
    /// is the same behind any.
    pub(super) fn made(
        &mut self,
        (segment_id, path): (String, std::path::PathBuf),
        behind: (&[String], &SegmentSet),
    ) -> Result<Made> {
        let (view, limits, pack) = (self.view, self.limits, self.pack);
        let mut nonce = [0u8; 24];
        alt_os::random::fill(&mut nonce);
        let mut segments = behind.1.segments().to_vec();
        let mut stored = None;
        let mut writing = None;
        if !pack.is_empty() {
            let ingested = match &mut self.verified {
                Some(verified) => {
                    verified.behind(behind.1).map_err(segment_error)?;
                    verified
                }
                none => {
                    let decode = alt_server_telemetry::Phase::Decode;
                    let verified = alt_server_store::telemetry::observe(decode, || {
                        crate::git::verified(pack, behind.1, limits, self.room, self.led.as_deref())
                    })?;
                    none.insert(verified)
                }
            };
            if ingested.objects() > 0 {
                let (segment_id, path, bytes, inline) = match &mut self.spooled {
                    Some((id, path, file)) => {
                        // the pack is in its file already; the index goes behind it, in
                        // the place of one written for other segments
                        file.set_len(pack.len() as u64)?;
                        file.seek(std::io::SeekFrom::Start(pack.len() as u64))?;
                        let mut index = Vec::with_capacity(ingested.objects() * 96 + 2048);
                        ingested.write_index(&mut index).map_err(segment_error)?;
                        file.write_all(&index)?;
                        file.sync_all()?;
                        std::fs::File::open(path.parent().expect("segment directory"))?
                            .sync_all()?;
                        let bytes = (pack.len() + index.len()) as u64;
                        ((*id).clone(), (*path).clone(), bytes, None)
                    }
                    None => {
                        // the file of an attempt before this one has no row
                        if let Some(before) = self.created.take() {
                            let _ = std::fs::remove_file(before);
                        }
                        writing = Some(alt_server_store::writing(&path));
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(&path)?;
                        *self.created = Some(path.clone());
                        let mut content =
                            Vec::with_capacity(pack.len() + ingested.objects() * 96 + 2048);
                        content.extend_from_slice(pack);
                        ingested.write_index(&mut content).map_err(segment_error)?;
                        file.write_all(&content)?;
                        let bytes = content.len() as u64;
                        let inline = content.len() <= alt_server_store::INLINE_SEGMENT_BYTES;
                        if !inline {
                            file.sync_all()?;
                            std::fs::File::open(path.parent().expect("segment directory"))?
                                .sync_all()?;
                        }
                        (segment_id, path, bytes, inline.then_some(content))
                    }
                };
                let segment = Arc::new(Segment::open(&path).map_err(segment_error)?);
                segments.push(segment.clone());
                let held = HeldSegment {
                    id: segment_id,
                    objects: ingested.objects() as u64,
                    bytes,
                    flags: ingested.flags(),
                    inline,
                };
                stored = Some((held, segment));
            }
        }
        let set = SegmentSet::new(segments);
        let refs = self.updates(&set)?;
        let key = RequestKey {
            scope: view.scope.clone(),
            actor_id: view.actor_id.clone(),
            request_id: format!("git-{}", hex(&nonce)),
        };
        let manifest = Manifest {
            protocol_version: VERSION,
            object_format: ObjectFormat::Sha1,
            entries: vec![],
        };
        let publication = Publication {
            protocol_version: VERSION,
            key: key.clone(),
            attribution: view.attribution.clone(),
            object_format: ObjectFormat::Sha1,
            refs,
            record_roots: vec![],
            content_manifest: manifest.id(limits)?,
        };
        Ok(Made {
            key,
            publication,
            set,
            stored,
            _writing: writing,
        })
    }
}
