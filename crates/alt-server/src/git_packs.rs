//! Git smart HTTP pushes over verified pack segments: a push is verified once and stored
//! as one immutable segment published with its refs.

use crate::Output;
use alt_git_codec::{HashAlgo, ObjectId};
use alt_pack_segment::{Oid, Segment, SegmentSet};
use alt_remote_protocol::{Manifest, ObjectFormat, Publication, RefUpdate, RequestKey, VERSION};
use alt_server_store::{Credential, Error, NewSegment, PackView, Result, Store, ancestor};
use alt_wire::pkt;
use std::{
    collections::BTreeSet,
    io::{Read, Seek, Write},
    os::unix::fs::OpenOptionsExt,
    sync::Arc,
};

mod made;
mod published;
use published::{Spooled, publish, spool};

pub(crate) fn raw(oid: ObjectId) -> Oid {
    oid.as_bytes().try_into().expect("sha1 object id")
}

pub(crate) fn id(oid: &Oid) -> ObjectId {
    ObjectId::from_bytes(HashAlgo::Sha1, oid).expect("sha1 object id")
}

pub(crate) fn segment_error(error: alt_pack_segment::Error) -> Error {
    use alt_pack_segment::Error as E;
    match error {
        E::Io(error) => Error::Io(error),
        E::Limit(reason) => Error::Limit(reason),
        E::Busy(reason) => Error::Busy(reason),
        E::GivenUp => Error::Busy("the request is waited for no longer"),
        E::Unsafe(oid, what) => Error::Fsck(oid, what),
        E::Format(reason) | E::Corrupt(reason) => Error::Integrity(reason),
        E::Dangling(..) | E::MissingBase(_) | E::NotFound(_) => {
            Error::Integrity("pack references an unavailable object")
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn receive(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    source: &mut dyn Read,
    view: PackView,
    room: Option<crate::git::Verifying>,
) -> Result<Output> {
    let limits = store.limits();
    // Before a body too large for its buffer, git sends a lone flush packet to probe
    // authentication; it expects an empty success and then sends the real request.
    let mut probe = [0u8; 4];
    let mut filled = 0;
    while filled < probe.len() {
        match source.read(&mut probe[filled..])? {
            0 => break,
            count => filled += count,
        }
    }
    let mut rest = [0u8; 1];
    let more = if filled == probe.len() {
        source.read(&mut rest)?
    } else {
        0
    };
    if &probe[..filled] == b"0000" && more == 0 {
        return Ok(Output::Bytes {
            content_type: "application/x-git-receive-pack-result",
            bytes: Vec::new(),
        });
    }
    // the commands are read a few bytes at a time and the body is a file
    let mut replay = std::io::BufReader::new(
        std::io::Cursor::new([&probe[..filled], &rest[..more]].concat()).chain(source),
    );
    let source: &mut dyn Read = &mut replay;
    let head = alt_wire::push::parse_push_request_head_bounded(
        &mut (&mut *source).take(limits.request_bytes as u64),
        HashAlgo::Sha1,
        limits.ref_updates,
        8192,
    )
    .map_err(|_| Error::Integrity("Git receive commands"))?;
    if head.updates.is_empty() {
        return Err(Error::Integrity("empty Git push"));
    }
    // as in Git, capabilities the server does not know are ignored, not refused
    let mut names = BTreeSet::new();
    for update in &head.updates {
        alt_remote_protocol::validate_ref_name(&update.name)?;
        if !names.insert(&update.name) || (update.old.is_none() && update.new.is_none()) {
            return Err(Error::Integrity("invalid or duplicate Git update"));
        }
    }
    // A pack that fits with its row is handled in memory. A larger one goes straight
    // into its segment file and is read back through a mapping, so its size is never
    // held on the heap.
    let mut pack = Vec::new();
    let inline = alt_server_store::INLINE_SEGMENT_BYTES;
    (&mut *source)
        .take(inline as u64 + 1)
        .read_to_end(&mut pack)?;
    let mut spooled = None;
    if pack.len() > inline {
        spooled = Some(spool(store, repository, &pack, source, limits.total_bytes)?);
        pack = Vec::new();
    }
    let mut created = spooled.as_ref().map(|spooled| spooled.path.clone());
    let mut published = false;
    let result = match &mut spooled {
        Some(Spooled {
            id,
            path,
            file,
            pack,
            ..
        }) => publish(
            store,
            credential,
            repository,
            &view,
            &head,
            (pack, room),
            Some((id, path, file)),
            &mut created,
            &mut published,
        ),
        None => publish(
            store,
            credential,
            repository,
            &view,
            &head,
            (&pack, room),
            None,
            &mut created,
            &mut published,
        ),
    };
    // an unknown outcome may be a committed row: the sweep removes the file if it is not
    if !published
        && !matches!(result, Err(Error::OutcomeUnknown))
        && let Some(path) = &created
    {
        // nothing references an unpublished segment
        let _ = std::fs::remove_file(path);
    }
    let (request, outcome) = match result {
        Ok(request) => (request, Ok(())),
        Err(error) => (None, Err(error)),
    };
    let message = refused_for(&outcome);
    if !head.capabilities.iter().any(|cap| cap == "report-status") {
        outcome?;
        return Ok(Output::Bytes {
            content_type: "application/x-git-receive-pack-result",
            bytes: Vec::new(),
        });
    }
    let commands: Vec<_> = head
        .updates
        .iter()
        .map(|update| match &message {
            Some(message) => alt_wire::push::CommandStatus::Ng {
                name: update.name.clone(),
                reason: message.clone(),
            },
            None => alt_wire::push::CommandStatus::Ok(update.name.clone()),
        })
        .collect();
    let refused = match &outcome {
        Err(Error::Fsck(oid, what)) => Some(format!("error: object {}: {what}\n", hex(oid))),
        _ => None,
    };
    let unpack = refused
        .as_ref()
        .map_or(Ok(()), |_| Err("index-pack abnormal exit"));
    let mut report = Vec::new();
    alt_wire::push::encode_report_status(&mut report, unpack, &commands)?;
    let body = if head.capabilities.iter().any(|cap| cap == "side-band-64k") {
        // what fsck says on band 2, which the client shows after `remote:`; the
        // report on band 1, closed by a flush packet
        let mut framed = Vec::with_capacity(report.len() + 16);
        if let Some(said) = &refused {
            framed.extend_from_slice(format!("{:04x}", said.len() + 5).as_bytes());
            framed.push(2);
            framed.extend_from_slice(said.as_bytes());
        }
        for chunk in report.chunks(65515) {
            framed.extend_from_slice(format!("{:04x}", chunk.len() + 5).as_bytes());
            framed.push(1);
            framed.extend_from_slice(chunk);
        }
        pkt::write_flush(&mut framed)?;
        framed
    } else {
        report
    };
    match request {
        Some(request) => Ok(Output::GitReport {
            bytes: body,
            request,
        }),
        None => Ok(Output::Bytes {
            content_type: "application/x-git-receive-pack-result",
            bytes: body,
        }),
    }
}

/// What a Git client is told a push was refused for, where it was.
fn refused_for(outcome: &Result<()>) -> Option<String> {
    match outcome {
        Ok(()) => None,
        Err(Error::OutcomeUnknown) => {
            Some("outcome unknown; inspect remote refs and the server request".to_owned())
        }
        Err(Error::Unauthorized) => Some("access revoked".to_owned()),
        // the operator can raise a limit; the pusher has to know which one it was
        Err(Error::Limit(what)) => Some(format!("server limit exceeded: {what}")),
        Err(Error::Busy(_)) => {
            Some("the packs of other pushes are being verified; push again".to_owned())
        }
        // as Git answers a pack its fsck refuses
        Err(Error::Fsck(..)) => Some("unpacker error".to_owned()),
        // a ref was moved or deleted after the client read where it was
        Err(Error::Repository(alt_repository_service::Error::Precondition(_)))
        | Err(Error::Precondition(CHANGED | alt_server_store::REFS_MOVED)) => {
            Some("a ref changed since it was read; fetch and push again".to_owned())
        }
        Err(_) => Some("publication rejected; inspect current refs and policy".to_owned()),
    }
}

/// what a push is refused with where a ref it updates moved after it was read
const CHANGED: &str = "reference changed since it was read";
