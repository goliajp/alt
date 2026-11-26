//! What a fetch is answered with: the pack of the objects it asks for,
//! written and framed on disk.
//!
//! The objects go into the pack one at a time, and the response is sent
//! from a file: what a fetch takes in memory is its largest object, not
//! the repository. Whoever may read a repository may ask for all that its
//! refs reach, and for nothing else that is in its store.

use std::fs::File;
use std::io::{BufWriter, Write};

use alt_git_codec::{Commit, ObjectId, ObjectKind, Tag, Tree};
use alt_hash::KeyedIdSet;
use alt_repo::Repository;
use alt_wire::fetch::{
    FetchRequest, encode_acknowledgments, encode_fetch_response_packfile, encode_shallow_info,
};

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

/// The first object `req` wants that no ref of `repo` reaches. An object
/// that is in the store and under no ref is one of a push that was
/// refused, or of a branch that was deleted: it is not served.
///
/// What a clone and a fetch ask for are the objects the refs point at,
/// and those are told without a walk. The objects a partial clone left
/// out are asked for one by one, and are looked for from the refs down.
fn unreached(repo: &Repository, req: &FetchRequest, until: &Until) -> Res<Option<ObjectId>> {
    let tips: Vec<ObjectId> = repo.list_refs()?.into_iter().map(|r| r.1).collect();
    let mut seen: KeyedIdSet<ObjectId> = tips.iter().copied().collect();
    let mut sought: KeyedIdSet<ObjectId> = req.wants.iter().copied().collect();
    sought.retain(|want| !seen.contains(want));
    let algo = repo.algo()?;
    let mut ahead = tips;
    while let Some(oid) = ahead.pop() {
        if sought.is_empty() {
            break;
        }
        until.go_on()?;
        let Some(object) = repo.read_object(&oid)? else {
            continue;
        };
        let mut under = Vec::new();
        match object.kind {
            ObjectKind::Commit => {
                let commit = Commit::parse(&object.data)?;
                under.extend(commit.tree());
                under.extend(commit.parents());
            }
            ObjectKind::Tag => under.extend(Tag::parse(&object.data)?.object()),
            ObjectKind::Tree => {
                for entry in Tree::parse(&object.data, algo)?.entries {
                    match entry.mode.object_kind() {
                        ObjectKind::Tree => under.push(entry.oid),
                        // a file is not read to know that it is reached
                        ObjectKind::Blob => drop(sought.remove(&entry.oid)),
                        _ => {}
                    }
                }
            }
            ObjectKind::Blob => {}
        }
        for oid in under {
            if seen.insert(oid) {
                sought.remove(&oid);
                ahead.push(oid);
            }
        }
    }
    Ok(req.wants.iter().find(|want| sought.contains(want)).copied())
}

/// Those of the commits `req` says its sender has that a ref of `repo`
/// reaches. One that is in the store and under no ref is taken as one
/// the server does not know: left out of the pack as a commit both
/// sides have, it would tell its sender that it is there.
fn reached(repo: &Repository, req: &FetchRequest, until: &Until) -> Res<Vec<ObjectId>> {
    let tips: Vec<ObjectId> = repo.list_refs()?.into_iter().map(|r| r.1).collect();
    let mut seen: KeyedIdSet<ObjectId> = tips.iter().copied().collect();
    let mut sought: KeyedIdSet<ObjectId> = req.haves.iter().copied().collect();
    let mut reached: Vec<ObjectId> = Vec::new();
    reached.extend(req.haves.iter().filter(|have| seen.contains(have)));
    sought.retain(|have| !seen.contains(have));
    // one the store lacks is found under no ref: it is not looked for
    let mut held = Vec::new();
    for have in sought {
        until.go_on()?;
        held.extend(repo.read_object(&have)?.map(|_| have));
    }
    let mut sought: KeyedIdSet<ObjectId> = held.into_iter().collect();
    let mut ahead = tips;
    while let Some(oid) = ahead.pop() {
        if sought.is_empty() {
            break;
        }
        until.go_on()?;
        let Some(object) = repo.read_object(&oid)? else {
            continue;
        };
        let under: Vec<ObjectId> = match object.kind {
            ObjectKind::Commit => Commit::parse(&object.data)?.parents().collect(),
            ObjectKind::Tag => Tag::parse(&object.data)?.object().into_iter().collect(),
            _ => Vec::new(),
        };
        for oid in under {
            if seen.insert(oid) {
                if sought.remove(&oid) {
                    reached.push(oid);
                }
                ahead.push(oid);
            }
        }
    }
    Ok(reached)
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
/// The file is written within the room responses have between them, and has
/// its part of it until it is sent. It is written for as long as `until`
/// says, and the error is a [`Stopped`] where that is over.
///
/// Once nothing is left that answers it otherwise than with a pack, what
/// the file begins with up to the pack is given to `begun`: the pack is
/// what takes long to make.
pub(super) fn response(
    repo: &Repository,
    req: &FetchRequest,
    room: &Room,
    from: Option<std::net::IpAddr>,
    until: &Until,
    begun: &mut dyn FnMut(Vec<u8>),
) -> Res<Sent> {
    let dir = alt_os::temp::tempdir()?;
    let path = dir.path().join("response");
    let mut framed = BufWriter::new(room.writing(File::create(&path)?, from));
    let refused = |framed: &mut BufWriter<Written>, why: &str| {
        alt_wire::pkt::write_data(framed, format!("ERR upload-pack: {why}").as_bytes())
    };
    if let Some(oid) = unreached(repo, req, until)? {
        refused(&mut framed, &format!("not our ref {oid}"))?;
    } else {
        let common = reached(repo, req, until)?;
        let ready = !req.wait_for_done && !common.is_empty();
        if !req.done && !ready {
            encode_acknowledgments(&mut framed, &common, false)?;
        } else {
            match shallow::ends(repo, req, until)? {
                Err(why) => refused(&mut framed, &why)?,
                Ok(shallow) => {
                    let mut before = Vec::new();
                    if !req.done {
                        encode_acknowledgments(&mut before, &common, true)?;
                    }
                    if let Some(told) = &shallow.told {
                        encode_shallow_info(&mut before, told)?;
                    }
                    framed.write_all(&before)?;
                    // the line the pack's frames come behind
                    alt_wire::pkt::write_data(&mut before, b"packfile\n")?;
                    begun(before);
                    pack(repo, req, &shallow, &common, dir.path(), &mut framed, until)?;
                }
            }
        }
    }
    let written = framed.into_inner().map_err(|failed| failed.into_error())?;
    Ok(written.sent_from(File::open(&path)?))
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
    framed: &mut impl Write,
    until: &Until,
) -> Res<()> {
    #[cfg(debug_assertions)]
    failing()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fetch names the commits its sender has, a quarter of a million at
    /// the most, and each is looked for in the store before the refs are gone
    /// down for it. A fetch whose time is over looks for none: with none of
    /// them in the store there is nothing else it would have stopped at.
    #[test]
    fn what_a_fetch_says_it_has_is_looked_for_as_long_as_it_is_gone_on_with() {
        let dir = alt_os::temp::tempdir().unwrap();
        alt_testutil::make_repo(dir.path(), "sha1");
        let repo = Repository::discover(dir.path()).unwrap();
        let haves: Vec<ObjectId> = (1..=4u8)
            .map(|n| ObjectId::from_bytes(repo.algo().unwrap(), &[n; 20]).unwrap())
            .collect();
        let req = FetchRequest {
            haves,
            ..FetchRequest::default()
        };
        assert_eq!(reached(&repo, &req, &Until::whenever()).unwrap(), []);
        let over = Until {
            by: Some(std::time::Instant::now()),
            gone: None,
        };
        let stopped = reached(&repo, &req, &over).unwrap_err();
        let late = super::super::until::Stopped::Late;
        assert_eq!(stopped.downcast_ref(), Some(&late), "{stopped}");
    }
}
