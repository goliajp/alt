//! The pack of a push written as its segment and published with its refs.

use super::made::{Push, fresh};
use super::*;
use alt_server_store::HeldSegment;

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
    // kept from the sweep from before the first byte is written
    let mut writing = Some(alt_server_store::writing(&path));
    let mut written = || -> Result<Spooled> {
        file.write_all(first)?;
        let rest = limit.saturating_add(1).saturating_sub(first.len() as u64);
        let copied = std::io::copy(&mut source.take(rest), &mut file)?;
        if first.len() as u64 + copied > limit {
            return Err(Error::Limit("Git pack bytes"));
        }
        // synced as it is received: a publication writes the index behind it and
        // syncs the file, the last of them with the row of its repository held
        file.sync_data()?;
        // the file is new and only this request writes to it; the index it gets
        // later lies behind what is mapped here
        let pack = unsafe { alt_os::map::Mmap::map(&file) }?;
        Ok(Spooled {
            _writing: writing.take().expect("written once"),
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
/// meantime the segment is told anew behind the segments there are then, which
/// takes no second reading of the pack: the depth of its deltas and whether it
/// brings an object again are told of the segments it is published behind, not
/// of those it was first read against. A push that finds the segments moved
/// [`MOVED_MOST`] times is published with the row of its repository held from
/// before the segments are looked at, and is sent back by nothing after that.
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
    spooled: Option<(&String, &std::path::PathBuf, &mut std::fs::File)>,
    created: &mut Option<std::path::PathBuf>,
    published: &mut bool,
) -> Result<Option<RequestKey>> {
    let changes = changed(view, head)?;
    if changes.is_empty() {
        return Ok(None);
    }
    let limits = store.limits();
    let gone = room.as_ref().map(|room| &*room.given_up);
    let led = (view.unreachable)
        .then(|| store.led(view, gone))
        .transpose()?;
    let mut behind = (view.segment_ids.clone(), view.segments.clone());
    let mut push = Push {
        view,
        changes: &changes,
        pack,
        room: room.as_ref(),
        spooled,
        created,
        limits,
        verified: None,
        led,
    };
    for _ in 0..MOVED_MOST {
        let fresh = fresh(store, repository)?;
        let made = push.made(fresh, (&behind.0, &behind.1))?;
        let segment = made.stored.as_ref().map(|stored| NewSegment {
            id: &stored.0.id,
            objects: stored.0.objects,
            bytes: stored.0.bytes,
            flags: stored.0.flags,
            inline: stored.0.inline.as_deref(),
        });
        let graph = alt_server_store::SegmentGraph {
            set: &made.set,
            limits: limits.into(),
        };
        // Git has no durable client request id; the generated one records the server
        // transaction for inspection, not for replay
        let committed =
            alt_server_store::telemetry::observe(alt_server_telemetry::Phase::Commit, || {
                let read = (&behind.0[..], &view.refs);
                store.pack_publish(credential, &made.publication, &graph, segment, read)
            });
        match committed {
            Err(Error::Precondition(alt_server_store::SEGMENTS_MOVED)) => {
                behind = store.segments_now(repository)?;
            }
            Err(error) => return Err(error),
            Ok(_) => return made.adopted(store, repository, published),
        }
    }
    // the segments moved under it time after time: no other is let by now
    let fresh = fresh(store, repository)?;
    let mut last = None;
    alt_server_store::telemetry::observe(alt_server_telemetry::Phase::Commit, || {
        store.pack_publish_held(credential, (repository, &view.refs), &mut |ids, set| {
            let mut made = push.made(fresh.clone(), (ids, set))?;
            let segment = made.stored.as_mut().map(|stored| HeldSegment {
                inline: stored.0.inline.take(),
                id: stored.0.id.clone(),
                ..stored.0
            });
            let given = (made.publication.clone(), made.set.clone(), segment);
            last = Some(made);
            Ok(given)
        })
    })?;
    let made = last.expect("made where it was published");
    made.adopted(store, repository, published)
}

/// The updates of a push that change a ref. A client may list refs it does not
/// change: they take no part in the publication, but a stale expectation is
/// still a conflict.
fn changed<'a>(
    view: &PackView,
    head: &'a alt_wire::push::PushHead,
) -> Result<Vec<&'a alt_wire::push::RefUpdate>> {
    let mut changes = Vec::new();
    for update in &head.updates {
        if update.old != update.new {
            changes.push(update);
        } else if view.refs.get(&update.name) != update.old {
            return Err(Error::Precondition(super::CHANGED));
        }
    }
    Ok(changes)
}

/// How often a push is published anew behind segments that moved before the
/// row of its repository is held for it.
const MOVED_MOST: usize = 3;
