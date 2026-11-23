//! What a fetch is answered with: the pack of the objects it asks for,
//! written and framed on disk.
//!
//! The objects go into the pack one at a time, and the response is sent
//! from a file: what a fetch takes in memory is its largest object, not
//! the repository. Whoever may read a repository may ask for all of it.

use std::fs::File;
use std::io::{BufWriter, Write};

use alt_git_codec::{ObjectId, ObjectKind};
use alt_repo::Repository;
use alt_wire::fetch::{FetchRequest, encode_fetch_response_packfile};

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

/// The response to `req`, in a file that has no name any more: the
/// want/have closure of the request, less what its filter leaves out, as
/// a pack in side-band frames. The pack is the wire payload and is not
/// kept.
pub(super) fn response(repo: &Repository, req: &FetchRequest) -> Res<File> {
    let outgoing = repo.reachable_objects(&req.wants, &req.haves)?;
    let filter = parse_filter_spec(req.filter.as_deref());
    // only a limit on the size of blobs has an object read to tell
    // whether it goes
    let sized = filter.blob_limit.is_some() && !filter.omit_blobs && !filter.omit_trees;
    let mut going = Vec::new();
    for (oid, kind) in &outgoing {
        let bytes = match sized && *kind == ObjectKind::Blob {
            true => read(repo, oid)?.data.len(),
            false => 0,
        };
        if !filter.excludes(*kind, bytes) {
            going.push((*oid, *kind));
        }
    }
    let dir = alt_os::temp::tempdir()?;
    let path = dir.path().join("response");
    let mut framed = BufWriter::new(File::create(&path)?);
    if going.is_empty() {
        encode_fetch_response_packfile(&mut framed, &[])?;
    } else {
        let count = u32::try_from(going.len())
            .map_err(|_| "outgoing object set exceeds u32 (server-side)")?;
        let mut writer = alt_git_pack::PackWriter::create(dir.path(), repo.algo()?, count)?;
        for (oid, kind) in going {
            writer.add(oid, kind, &read(repo, &oid)?.data)?;
        }
        let pack = File::open(writer.finish()?.pack_path)?;
        // the file is this request's own and is not written to again
        let pack = unsafe { alt_os::map::Mmap::map(&pack)? };
        encode_fetch_response_packfile(&mut framed, &pack)?;
    }
    framed.flush()?;
    drop(framed);
    Ok(File::open(&path)?)
}
