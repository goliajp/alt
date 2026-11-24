//! The pack of a push written as its segment and published with its refs.

use super::*;

/// a pack that was written to its segment file as it arrived
pub(super) struct Spooled {
    /// kept from the sweep until the push is answered
    _writing: alt_server_store::Writing,
    pub(super) id: String,
    pub(super) path: std::path::PathBuf,
    pub(super) file: std::fs::File,
    pub(super) pack: alt_os::map::Mmap,
}

/// Writes `first` and the rest of `source` to a new segment file and maps it.
pub(super) fn spool(
    store: &Store,
    repository: &str,
    first: &[u8],
    source: &mut dyn Read,
    limit: u64,
) -> Result<Spooled> {
    let mut nonce = [0u8; 16];
    alt_os::random::fill(&mut nonce);
    let id = hex(&nonce);
    let path = store.pack_path(repository, &id)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let mut written = || -> Result<Spooled> {
        file.write_all(first)?;
        let rest = limit.saturating_add(1).saturating_sub(first.len() as u64);
        let copied = std::io::copy(&mut source.take(rest), &mut file)?;
        if first.len() as u64 + copied > limit {
            return Err(Error::Limit("Git pack bytes"));
        }
        // the file is new and only this request writes to it; the index it gets
        // later lies behind what is mapped here
        let pack = unsafe { alt_os::map::Mmap::map(&file) }?;
        Ok(Spooled {
            _writing: alt_server_store::writing(&path),
            id: id.clone(),
            path: path.clone(),
            file: file.try_clone()?,
            pack,
        })
    };
    let spooled = written();
    if spooled.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    spooled
}

/// Publishes the updates of `head` with the segment of `pack`, which is verified
/// behind the segments of `view`. Where a push or a merge was published in the
/// meantime the pack is verified again behind the segments there are then: the
/// depth of its deltas and whether it brings an object again are told of the
/// segments it is published behind, not of those it was first read against.
#[expect(
    clippy::too_many_arguments,
    reason = "one caller; the parts of one request"
)]
pub(super) fn publish(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    view: &PackView,
    head: &alt_wire::push::PushHead,
    (pack, room): (&[u8], Option<crate::git::Verifying>),
    mut spooled: Option<(&String, &std::path::PathBuf, &mut std::fs::File)>,
    created: &mut Option<std::path::PathBuf>,
    published: &mut bool,
) -> Result<Option<RequestKey>> {
    // A client may list refs it does not change. They take no part in the publication,
    // but a stale expectation is still a conflict.
    let mut changes = Vec::new();
    for update in &head.updates {
        if update.old != update.new {
            changes.push(update);
        } else if view.refs.get(&update.name) != update.old {
            return Err(Error::Precondition("reference changed since it was read"));
        }
    }
    if changes.is_empty() {
        return Ok(None);
    }
    let mut behind = (view.segment_ids.clone(), view.segments.clone());
    loop {
        let spooled = spooled
            .as_mut()
            .map(|(id, path, file)| (*id, *path, &mut **file));
        let written = (spooled, &mut *created, &mut *published);
        let pack = (pack, room.as_ref());
        match once(store, credential, view, &changes, pack, written, &behind) {
            Err(Error::Precondition(alt_server_store::SEGMENTS_MOVED)) => {
                behind = store.segments_now(repository)?;
            }
            done => return done,
        }
    }
}

/// The file a pack came in, where it has one; the file made for it here, if
/// any; and whether its segment was published.
type Written<'a> = (
    Option<(&'a String, &'a std::path::PathBuf, &'a mut std::fs::File)>,
    &'a mut Option<std::path::PathBuf>,
    &'a mut bool,
);

/// [`publish`] behind the segments `behind`, with their ids.
fn once(
    store: &mut Store,
    credential: &Credential,
    view: &PackView,
    changes: &[&alt_wire::push::RefUpdate],
    (pack, room): (&[u8], Option<&crate::git::Verifying>),
    (spooled, created, published): Written<'_>,
    behind: &(Vec<String>, SegmentSet),
) -> Result<Option<RequestKey>> {
    let limits = store.limits();
    let repository = &view.scope.repository_instance_id;
    let mut nonce = [0u8; 40];
    alt_os::random::fill(&mut nonce);
    let mut segments = behind.1.segments().to_vec();
    let mut stored = None;
    // a file made here is kept from the sweep until its row is committed
    let mut writing = None;
    if !pack.is_empty() {
        let ingested =
            alt_server_store::telemetry::observe(alt_server_telemetry::Phase::Decode, || {
                crate::git::verified(pack, &behind.1, limits, room)
            })?;
        if ingested.objects() > 0 {
            let (segment_id, path, bytes, inline) = match spooled {
                Some((id, path, file)) => {
                    // the pack is in its file already; the index goes behind it, in
                    // the place of one written for other segments
                    file.set_len(pack.len() as u64)?;
                    file.seek(std::io::SeekFrom::Start(pack.len() as u64))?;
                    let mut index = Vec::with_capacity(ingested.objects() * 96 + 2048);
                    ingested.write_index(&mut index).map_err(segment_error)?;
                    file.write_all(&index)?;
                    file.sync_all()?;
                    std::fs::File::open(path.parent().expect("segment directory"))?.sync_all()?;
                    let bytes = (pack.len() + index.len()) as u64;
                    (id.clone(), path.clone(), bytes, None)
                }
                None => {
                    // the file of an attempt before this one has no row
                    if let Some(before) = created.take() {
                        let _ = std::fs::remove_file(before);
                    }
                    let segment_id = hex(&nonce[..16]);
                    let path = store.pack_path(repository, &segment_id)?;
                    writing = Some(alt_server_store::writing(&path));
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&path)?;
                    *created = Some(path.clone());
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
            stored = Some((
                segment_id,
                ingested.objects() as u64,
                ingested.flags(),
                segment,
                inline,
                bytes,
            ));
        }
    }
    let set = SegmentSet::new(segments);
    let mut remaining = limits.walk_steps;
    let mut refs = Vec::new();
    for update in changes.iter().copied() {
        let force = match (update.old, update.new) {
            (Some(old), Some(new)) => {
                update.name.starts_with("refs/tags/")
                    || !ancestor(&set, &raw(old), &raw(new), &mut remaining)?
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
    let key = RequestKey {
        scope: view.scope.clone(),
        actor_id: view.actor_id.clone(),
        request_id: format!("git-{}", hex(&nonce[16..])),
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
    let graph = alt_server_store::SegmentGraph {
        set: &set,
        limits: limits.into(),
    };
    let segment = stored
        .as_ref()
        .map(|(id, objects, flags, _, inline, bytes)| NewSegment {
            id,
            objects: *objects,
            bytes: *bytes,
            flags: *flags,
            inline: inline.as_deref(),
        });
    // Git has no durable client request id; the generated one records the server
    // transaction for inspection, not for replay
    alt_server_store::telemetry::observe(alt_server_telemetry::Phase::Commit, || {
        store.pack_publish(credential, &publication, &graph, segment, &behind.0)
    })?;
    drop(writing);
    *published = stored.is_some();
    if let Some((id, _, _, segment, _, _)) = stored {
        store.adopt_segment(repository, &id, segment)?;
    }
    Ok(Some(key))
}
