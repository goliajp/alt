//! What a fetch is answered with: the pack of the objects it asks for,
//! written and framed on disk.
//!
//! The objects go into the pack one at a time, and the response is sent
//! from a file: what a fetch takes in memory is its largest object, not
//! the repository. Whoever may read a repository may ask for all that its
//! refs reach, and for nothing else that is in its store.

use std::fs::File;
use std::io::{BufWriter, Write};
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
use filter::parse_filter_spec;

#[path = "fetch/filter.rs"]
mod filter;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

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
///
/// Once nothing is left that answers it otherwise than with a pack, what
/// the file begins with up to the pack is given to `begun`: the pack is
/// what takes long to make. That is done once, and a response built again
/// after it begins as it was told to.
pub(super) fn response(
    repo: &Repository,
    req: &FetchRequest,
    room: &Room,
    from: (Option<IpAddr>, &Arc<AtomicBool>),
    until: &Until,
    begun: &mut dyn FnMut(Vec<u8>),
) -> Res<Sent> {
    let mut aside = Some(super::workers::Workers::aside(Arc::clone(from.1), until.by));
    let mut taken = None;
    let time = until
        .by
        .map(|by| by.saturating_duration_since(Instant::now()));
    let gone = until.gone;
    let mut again = None;
    let mut ahead = None;
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
        let wrote = built(
            repo,
            req,
            (dir.path(), &mut framed),
            until,
            (&mut ahead, begun),
        );
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

/// What a response that is to have a pack begins with, as it was given to
/// `begun`: one built again after its turn begins with the same, so that
/// what went out ahead of the pack still holds.
struct Ahead {
    common: Vec<ObjectId>,
    shallow: Shallow,
    before: Vec<u8>,
}

/// [`response`] written into `framed`, with what it is built from under
/// `dir`. Where `ahead` was given to `begun` already it is not worked out
/// again.
fn built(
    repo: &Repository,
    req: &FetchRequest,
    (dir, framed): (&std::path::Path, &mut BufWriter<Written>),
    until: &Until,
    (ahead, begun): (&mut Option<Ahead>, &mut dyn FnMut(Vec<u8>)),
) -> Res<()> {
    if let Some(ahead) = ahead {
        framed.write_all(&ahead.before)?;
        return pack(repo, req, ahead, dir, framed, until);
    }
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
    let shallow = match shallow::ends(repo, req, until)? {
        Err(why) => return Ok(refused(framed, &why)?),
        Ok(shallow) => shallow,
    };
    let mut before = Vec::new();
    if !req.done {
        encode_acknowledgments(&mut before, &common, true)?;
    }
    if let Some(told) = &shallow.told {
        encode_shallow_info(&mut before, told)?;
    }
    framed.write_all(&before)?;
    let mut head = before.clone();
    // the line the pack's frames come behind
    alt_wire::pkt::write_data(&mut head, b"packfile\n")?;
    begun(head);
    let ahead = ahead.insert(Ahead {
        common,
        shallow,
        before,
    });
    pack(repo, req, ahead, dir, framed, until)
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

/// The objects `req` asks for beyond what is `ahead`, as a pack written
/// under `dir` and framed into `framed`.
fn pack(
    repo: &Repository,
    req: &FetchRequest,
    ahead: &Ahead,
    dir: &std::path::Path,
    framed: &mut BufWriter<Written>,
    until: &Until,
) -> Res<()> {
    #[cfg(debug_assertions)]
    failing()?;
    let mut roots = req.wants.clone();
    roots.extend(&ahead.shallow.parents);
    let go_on = || until.go_on().is_ok();
    let (common, ends) = (&ahead.common, &ahead.shallow.ends);
    let outgoing = repo.reachable_named_while(&roots, common, ends, &go_on);
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

/// Where a debug build is told to, the pack of a fetch fails as one that
/// an error of the server stops: with `ALT_SERVER_PACK_FAILS=<file>`, the
/// file is made, and once it is removed the pack fails with an error that
/// names it. A release build holds none of this.
#[cfg(debug_assertions)]
fn failing() -> Res<()> {
    let Some(file) = std::env::var_os("ALT_SERVER_PACK_FAILS") else {
        return Ok(());
    };
    let file = std::path::PathBuf::from(file);
    std::fs::write(&file, "")?;
    while file.exists() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let gone = File::open(&file).expect_err("the file is removed");
    Err(format!("{}: {gone}", file.display()).into())
}
