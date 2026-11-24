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

fn read(repo: &Repository, oid: &ObjectId) -> Res<alt_git_codec::RawObject> {
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
fn unreached(repo: &Repository, req: &FetchRequest) -> Res<Option<ObjectId>> {
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
fn reached(repo: &Repository, req: &FetchRequest) -> Res<Vec<ObjectId>> {
    let tips: Vec<ObjectId> = repo.list_refs()?.into_iter().map(|r| r.1).collect();
    let mut seen: KeyedIdSet<ObjectId> = tips.iter().copied().collect();
    let mut sought: KeyedIdSet<ObjectId> = req.haves.iter().copied().collect();
    let mut reached: Vec<ObjectId> = Vec::new();
    reached.extend(req.haves.iter().filter(|have| seen.contains(have)));
    sought.retain(|have| !seen.contains(have));
    // one the store lacks is found under no ref: it is not looked for
    let mut held = KeyedIdSet::default();
    for have in &sought {
        if repo.read_object(have)?.is_some() {
            held.insert(*have);
        }
    }
    let mut sought = held;
    let mut ahead = tips;
    while let Some(oid) = ahead.pop() {
        if sought.is_empty() {
            break;
        }
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
/// its part of it until it is sent.
pub(super) fn response(
    repo: &Repository,
    req: &FetchRequest,
    room: &Room,
    from: Option<std::net::IpAddr>,
) -> Res<Sent> {
    let dir = alt_os::temp::tempdir()?;
    let path = dir.path().join("response");
    let mut framed = BufWriter::new(room.writing(File::create(&path)?, from));
    let refused = |framed: &mut BufWriter<Written>, why: &str| {
        alt_wire::pkt::write_data(framed, format!("ERR upload-pack: {why}").as_bytes())
    };
    if let Some(oid) = unreached(repo, req)? {
        refused(&mut framed, &format!("not our ref {oid}"))?;
    } else {
        let common = reached(repo, req)?;
        let ready = !req.wait_for_done && !common.is_empty();
        if !req.done && !ready {
            encode_acknowledgments(&mut framed, &common, false)?;
        } else {
            match shallow::ends(repo, req)? {
                Err(why) => refused(&mut framed, &why)?,
                Ok(shallow) => {
                    if !req.done {
                        encode_acknowledgments(&mut framed, &common, true)?;
                    }
                    if let Some(told) = &shallow.told {
                        encode_shallow_info(&mut framed, told)?;
                    }
                    pack(repo, req, &shallow, &common, dir.path(), &mut framed)?;
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
) -> Res<()> {
    let mut roots = req.wants.clone();
    roots.extend(&shallow.parents);
    let outgoing = repo.reachable_named_between(&roots, common, &shallow.ends)?;
    let filter = parse_filter_spec(req.filter.as_deref());
    // only a limit on the size of blobs has an object read to tell
    // whether it goes
    let sized = filter.blob_limit.is_some() && !filter.omit_blobs && !filter.omit_trees;
    let asked: KeyedIdSet<ObjectId> = req.wants.iter().copied().collect();
    let mut going = Vec::new();
    for at in outgoing {
        let bytes = match sized && at.kind == ObjectKind::Blob {
            true => read(repo, &at.oid)?.data.len(),
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
        writer.add(at.oid, at.kind, &read(repo, &at.oid)?.data)?;
    }
    let pack = File::open(writer.finish()?.pack_path)?;
    // the file is this request's own and is not written to again
    let pack = unsafe { alt_os::map::Mmap::map(&pack)? };
    Ok(encode_fetch_response_packfile(framed, &pack)?)
}
