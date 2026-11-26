//! What a fetch is answered with: the pack of the objects it asks for,
//! written and framed on disk.
//!
//! The objects go into the pack one at a time, and the response is sent
//! from a file: what a fetch takes in memory is its largest object, not
//! the repository. Whoever may read a repository may ask for all that its
//! refs reach, and for nothing else that is in its store.

use std::fs::File;
use std::io::BufWriter;
use std::time::Instant;
use std::{net::IpAddr, sync::Arc, sync::atomic::AtomicBool};

use alt_git_codec::{ObjectId, ObjectKind};
use alt_hash::KeyedIdSet;
use alt_repo::Repository;
use alt_wire::fetch::{
    FetchRequest, encode_acknowledgments, encode_fetch_response_packfile, encode_shallow_info,
};

use super::reached::{reached, unreached};
use super::sending::{Room, Sent, Written};
use super::shallow::{self, Shallow};
pub(super) use super::until::{Stopped, Until};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// The partial-clone filter shapes [`response`] currently
/// honours. Everything else parses as `None` (no filter) for
/// forward-compatibility — unknown filters degrade to a full pack
/// rather than rejecting the fetch, since the wire spec lets the
/// server be permissive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FilterSpec {
    omit_blobs: bool,
    blob_limit: Option<usize>,
    omit_trees: bool,
}

impl FilterSpec {
    /// Whether an object of `kind` and of `bytes` bytes is left out.
    fn excludes(&self, kind: ObjectKind, bytes: usize) -> bool {
        match kind {
            ObjectKind::Blob => {
                if self.omit_blobs || self.omit_trees {
                    return true;
                }
                if let Some(limit) = self.blob_limit
                    && bytes >= limit
                {
                    return true;
                }
                false
            }
            ObjectKind::Tree => self.omit_trees,
            _ => false,
        }
    }
}

fn parse_filter_spec(raw: Option<&str>) -> FilterSpec {
    let mut out = FilterSpec::default();
    let Some(spec) = raw else {
        return out;
    };
    let spec = spec.trim();
    match spec {
        "blob:none" => out.omit_blobs = true,
        "tree:0" => {
            out.omit_trees = true;
            out.omit_blobs = true; // tree:0 implies blob:none semantically
        }
        _ => {
            if let Some(n) = spec.strip_prefix("blob:limit=")
                && let Ok(limit) = n.parse::<usize>()
            {
                out.blob_limit = Some(limit);
            }
            // unknown / unsupported filters silently degrade to "send
            // everything" — git's spec lets the server be permissive.
        }
    }
    out
}

fn read(repo: &Repository, oid: &ObjectId, until: &Until) -> Res<alt_git_codec::RawObject> {
    until.go_on()?;
    let read = repo.read_object(oid)?;
    Ok(read.ok_or_else(|| format!("outgoing object {oid} missing from server odb"))?)
}

/// The response to `req`, in a file that has no name any more: the
/// want/have closure of the request, less what its filter and the ends
/// of its history leave out, as a pack in side-band frames. The pack is
/// the wire payload and is not kept. A request for what no ref reaches is
/// answered as `git upload-pack` answers a want it does not serve.
///
/// A request that does not say `done` is told which of its `have` lines
/// the server has too. It gets its pack in the same response once one of
/// them is, and is otherwise left to send more of them or `done`: the
/// pack then leaves out what the commits acknowledged so far reach, which
/// may be less than a longer negotiation would have found.
///
/// The file has its part of the room responses have until it is sent. A response that would be
/// large and would wait for its turn is dropped with what it was built from, and built again
/// once the turn is had, which is waited for with the worker given back meanwhile. It is written
/// for as long as `until` says, and the error is a [`Stopped`] where that is over; one built
/// again has as long again from when it has its turn.
pub(super) fn response(
    repo: &Repository,
    req: &FetchRequest,
    room: &Room,
    from: (Option<IpAddr>, &Arc<AtomicBool>),
    until: &Until,
) -> Res<Sent> {
    let mut aside = Some(super::workers::Workers::aside(Arc::clone(from.1), until.by));
    let mut taken = None;
    let time = until
        .by
        .map(|by| by.saturating_duration_since(Instant::now()));
    let gone = until.gone;
    let mut again = None;
    loop {
        let until = again.as_ref().unwrap_or(until);
        let dir = alt_os::temp::tempdir()?;
        let path = dir.path().join("response");
        let file = File::create(&path)?;
        let written = match taken.take() {
            Some(taken) => Written::again(file, taken),
            None => room.writing(file, from, until.by, aside.take()),
        };
        let mut framed = BufWriter::new(written);
        let wrote = built(repo, req, dir.path(), &mut framed, until);
        if framed.get_ref().put_off() {
            drop(wrote);
            let (written, _) = framed.into_parts();
            drop(dir);
            taken = Some(written.turn()?);
            again = Some(Until {
                by: time.map(|time| Instant::now() + time),
                gone,
            });
            continue;
        }
        wrote?;
        let written = framed.into_inner().map_err(|failed| failed.into_error())?;
        return Ok(written.sent_from(File::open(&path)?));
    }
}

/// [`response`] written into `framed`, with what it is built from under `dir`.
fn built(
    repo: &Repository,
    req: &FetchRequest,
    dir: &std::path::Path,
    framed: &mut BufWriter<Written>,
    until: &Until,
) -> Res<()> {
    let refused = |framed: &mut BufWriter<Written>, why: &str| {
        alt_wire::pkt::write_data(framed, format!("ERR upload-pack: {why}").as_bytes())
    };
    if let Some(oid) = unreached(repo, req, until)? {
        return Ok(refused(framed, &format!("not our ref {oid}"))?);
    }
    let common = reached(repo, req, until)?;
    let ready = !req.wait_for_done && !common.is_empty();
    if !req.done && !ready {
        return Ok(encode_acknowledgments(framed, &common, false)?);
    }
    match shallow::ends(repo, req, until)? {
        Err(why) => refused(framed, &why)?,
        Ok(shallow) => {
            if !req.done {
                encode_acknowledgments(framed, &common, true)?;
            }
            if let Some(told) = &shallow.told {
                encode_shallow_info(framed, told)?;
            }
            pack(repo, req, &shallow, &common, dir, framed, until)?;
        }
    }
    Ok(())
}

/// What a response looks for deltas with. It is the memory of one
/// request, beside its largest object: the last few objects of a kind, and
/// none over a size that is told apart from the rest of a file by more
/// than its neighbours in history.
const DELTAS: alt_git_pack::DeltaOptions = alt_git_pack::DeltaOptions {
    window: 3,
    memory_bytes: 32 << 20,
    max_object_bytes: 8 << 20,
    max_depth: 32,
};

/// The objects `req` asks for, as a pack written under `dir` and framed
/// into `framed`.
fn pack(
    repo: &Repository,
    req: &FetchRequest,
    shallow: &Shallow,
    common: &[ObjectId],
    dir: &std::path::Path,
    framed: &mut BufWriter<Written>,
    until: &Until,
) -> Res<()> {
    let mut roots = req.wants.clone();
    roots.extend(&shallow.parents);
    let go_on = || until.go_on().is_ok();
    let outgoing = repo.reachable_named_while(&roots, common, &shallow.ends, &go_on);
    // why it was stopped, where it was
    until.go_on()?;
    let outgoing = outgoing?;
    let filter = parse_filter_spec(req.filter.as_deref());
    // only a limit on the size of blobs has an object read to tell
    // whether it goes
    let sized = filter.blob_limit.is_some() && !filter.omit_blobs && !filter.omit_trees;
    let asked: KeyedIdSet<ObjectId> = req.wants.iter().copied().collect();
    let mut going = Vec::new();
    for at in outgoing {
        let bytes = match sized && at.kind == ObjectKind::Blob {
            true => read(repo, &at.oid, until)?.data.len(),
            false => 0,
        };
        // what is asked for by its id goes whatever the filter: it is how
        // a partial clone gets what it left out
        if asked.contains(&at.oid) || !filter.excludes(at.kind, bytes) {
            going.push(at);
        }
    }
    // the versions of a file follow each other, and each is written as
    // what it has that the ones before it have not
    going.sort_by_key(|at| (at.kind as u8, at.name));
    let count =
        u32::try_from(going.len()).map_err(|_| "outgoing object set exceeds u32 (server-side)")?;
    let mut writer =
        alt_git_pack::PackWriter::create_with_deltas(dir, repo.algo()?, count, DELTAS)?;
    for at in going {
        writer.add(at.oid, at.kind, &read(repo, &at.oid, until)?.data)?;
    }
    let pack = File::open(writer.finish()?.pack_path)?;
    // the file is this request's own and is not written to again
    let pack = unsafe { alt_os::map::Mmap::map(&pack)? };
    // a large one has its turn before it is framed, or is put off: the side
    // band adds five bytes to each packet of 65515
    let framing = (pack.len() as u64 / 65515 + 2) * 5 + 64;
    framed.get_mut().expect(pack.len() as u64 + framing)?;
    Ok(encode_fetch_response_packfile(framed, &pack)?)
}
