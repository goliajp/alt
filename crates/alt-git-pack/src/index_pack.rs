//! `git index-pack` equivalent: given a `.pack` file on disk, produce the
//! sibling `.idx` so [`IndexedPack`](crate::IndexedPack) can open it.
//!
//! ## Why this module
//!
//! Packs received from a smart-http fetch arrive as a *stream*: the wire
//! sends bytes, we save them to disk, and now we have a pack with no idx.
//! The idx is what makes random object lookup possible (and what every
//! downstream — alt-import, alt-export, lineage delta — depends on).
//! `git index-pack` builds it; we need our own.
//!
//! ## Scope
//!
//! A pack that was fetched holds the base of every delta it has: we don't
//! claim the `thin-pack` capability. A pack that was pushed is thin, and
//! is made whole from the store it was pushed to ([`index_pack_thin`]).
//!
//! ## Algorithm
//!
//! As `git index-pack` has it, in two passes over the mapped pack.
//!
//! 1. The entries are walked in file order ([`scan`]). Each is inflated to
//!    find where it ends and to checksum its bytes; a plain entry is
//!    hashed to its id there, and nothing of any entry is kept but its
//!    place, its form and what it names as its base.
//! 2. The deltas are sorted by their bases, by entry for an `OFS_DELTA`
//!    and by id for a `REF_DELTA`, and resolved depth first from every
//!    plain entry ([`resolve`]): once an object is known, the deltas made
//!    of it are found by a search of the sorted lists. Every delta is
//!    inflated again, applied and hashed once, in whichever order the
//!    pack has its entries.
//!
//! ## Memory
//!
//! What is held is a few words for each entry, the bases on the path
//! from a plain entry to the delta being resolved, and the one result.
//! The bases are held within [`IndexLimits::base_bytes`], git's
//! `core.deltaBaseCacheLimit`: beyond it those nearest the plain entry
//! are let go of and made again from the pack when another delta of
//! theirs comes up. What is made again counts against
//! [`IndexLimits::inflated_bytes`], and a chain is refused past
//! [`MAX_DELTA_DEPTH`].

mod budget;
mod deltas;
mod files;
mod held;
mod idx;
mod kept;
mod limits;
mod memory;
mod objects;
mod resolve;
mod scan;
mod thin;
mod walk;

pub use budget::{Budget, IndexWork, Objects as ObjectMemory, SPILL_FILES};
pub use deltas::{Deltas, Of};
pub use files::{BaseFiles, FILES as BASE_FILES};
pub use limits::IndexLimits;
pub use memory::{Memory, Taken, Wait};
pub use objects::Objects;
pub use thin::Bases;
pub use walk::Walk;

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use alt_git_codec::{HashAlgo, ObjectId};

use crate::{Pack, PackError};

/// The deepest chain of deltas a pack may hold, the deepest
/// `git pack-objects` makes.
pub const MAX_DELTA_DEPTH: u32 = 4095;

/// What a pack has for the objects it inflates and makes one at a time
/// without asking for it: the objects of a history are far smaller, and
/// only one that is larger has memory taken for it alone.
pub const EACH: u64 = 4 << 20;

/// Result of indexing a `.pack` file.
#[derive(Debug)]
pub struct IndexedPackOnDisk {
    /// Path of the pack we read (possibly renamed to its content-derived
    /// `pack-<trailer-hex>.pack` form when [`index_pack`] was called with
    /// `rename = true`).
    pub pack_path: PathBuf,
    /// Path of the newly-written `.idx`.
    pub idx_path: PathBuf,
    /// Pack trailer (file-name stem when renamed).
    pub trailer: Vec<u8>,
    /// Object count.
    pub objects: u32,
    pub work: IndexWork,
    /// the memory the pack took, held for as long as its objects are read
    pub memory: Option<Taken<'static>>,
}

/// Build a `.idx` for an existing `.pack` file. The idx is written next
/// to the pack (`<pack>.idx`). When `rename` is true, both files are then
/// renamed to git's conventional `pack-<trailer-hex>.{pack,idx}` form.
///
/// `algo` must match the algorithm used for object hashing in this pack
/// (sha-1 unless the repo is explicitly sha-256).
pub fn index_pack(
    pack_path: &Path,
    algo: HashAlgo,
    rename: bool,
) -> Result<IndexedPackOnDisk, PackError> {
    index_pack_within(pack_path, algo, rename, &IndexLimits::default())
}

/// [`index_pack`] of a pack that whoever sent it chose the content of.
pub fn index_pack_within(
    pack_path: &Path,
    algo: HashAlgo,
    rename: bool,
    limits: &IndexLimits<'_>,
) -> Result<IndexedPackOnDisk, PackError> {
    index_pack_thin(pack_path, algo, rename, limits, None)
}

/// [`index_pack_within`] of a pack that may be thin: the bases it leaves
/// out are read from `bases` and appended to it. Its trailer and its
/// count of objects are those of the pack made whole then.
pub fn index_pack_thin(
    pack_path: &Path,
    algo: HashAlgo,
    rename: bool,
    limits: &IndexLimits<'_>,
    bases: Option<Bases<'_>>,
) -> Result<IndexedPackOnDisk, PackError> {
    // the file is not modified for the lifetime of a map: we hold the
    // only handle
    let pack = Pack::open(pack_path, algo)?;
    let map = pack.bytes();
    let trailer_len = algo.raw_len();
    let count = pack.object_count();
    let mut trailer = map[map.len() - trailer_len..].to_vec();
    // verify the pack trailer matches a fresh hash of the body — protects
    // against truncated / corrupt downloads
    verify_trailer(map, algo, &trailer)?;

    let mut work = IndexWork::default();
    // an entry is inflated whole to be scanned: no larger than an object
    // may be, nor than what its stream can inflate to
    let body = (map.len() as u64).saturating_mul(1032);
    let scanning = limits.let_in(0, body.min(limits.object_bytes))?;
    let room = limits.objects(&scanning);
    let mut scanned = scan::scan(&pack, count, limits, room, &mut work)?;
    drop(scanning);
    let before = limits.resolving(scanned.inflated, scanned.largest);
    let mut memory = limits.let_in(before.0, before.1)?;
    let mut oids = scanned.oids.clone();
    let room = limits.objects(&memory);
    resolve::resolve(&pack, &mut scanned, &mut oids, 0, limits, room, &mut work)?;
    drop(pack);
    let came = oids.len();
    if let Some(bases) = bases
        && oids.contains(&None)
        && let Some(whole) =
            thin::complete(pack_path, algo, &mut scanned, &mut oids, bases, limits)?
    {
        trailer = whole;
        // a base that was appended may be larger than all the pack came with
        let after = limits.resolving(scanned.inflated, scanned.largest);
        if after != before {
            drop(memory.take());
            memory = limits.let_in(after.0, after.1)?;
        }
        let pack = Pack::open(pack_path, algo)?;
        let room = limits.objects(&memory);
        resolve::resolve(
            &pack,
            &mut scanned,
            &mut oids,
            came,
            limits,
            room,
            &mut work,
        )?;
    }
    let stuck = PackError::Format("delta resolution stuck (ref-delta base missing or cycle)");
    let oids: Vec<ObjectId> = oids.into_iter().collect::<Option<_>>().ok_or(stuck)?;
    let count = oids.len() as u32;
    let mut oid_entries: Vec<(ObjectId, u64, u32)> = scanned
        .entries
        .iter()
        .zip(oids)
        .map(|(entry, oid)| (oid, entry.offset, entry.crc32))
        .collect();
    drop(scanned);

    let idx_bytes = idx::build_idx(algo, &mut oid_entries, &trailer);
    let (final_pack, final_idx) = placed(pack_path, &idx_bytes, &trailer, rename, limits.kept)?;

    Ok(IndexedPackOnDisk {
        pack_path: final_pack,
        idx_path: final_idx,
        trailer,
        objects: count,
        work,
        memory,
    })
}

/// Writes `idx` next to the pack at `pack_path` and, with `rename`, gives
/// both the names of `trailer`; where they are then. Where that fails, the
/// idx is not left behind, and the pack is where it was unless a pack of
/// the same name, which is the same pack, was there before.
fn placed(
    pack_path: &Path,
    idx: &[u8],
    trailer: &[u8],
    rename: bool,
    kept: bool,
) -> Result<(PathBuf, PathBuf), PackError> {
    let idx_path = pack_path.with_extension("idx");
    let tmp_idx = pack_path.with_extension("idx.tmp");
    let written =
        write_idx(&tmp_idx, idx, kept).and_then(|()| std::fs::rename(&tmp_idx, &idx_path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp_idx);
        return Err(e.into());
    }
    if !rename {
        return Ok((pack_path.to_path_buf(), idx_path));
    }
    let dir = pack_path.parent().unwrap_or_else(|| Path::new("."));
    let hex: String = trailer.iter().map(|b| format!("{b:02x}")).collect();
    let renamed_pack = dir.join(format!("pack-{hex}.pack"));
    let renamed_idx = dir.join(format!("pack-{hex}.idx"));
    let there = std::fs::symlink_metadata(&renamed_pack).is_ok();
    let moved = (|| {
        if pack_path != renamed_pack {
            std::fs::rename(pack_path, &renamed_pack)?;
        }
        if idx_path != renamed_idx
            && let Err(e) = std::fs::rename(&idx_path, &renamed_idx)
        {
            // the pack goes back to whoever has it under its own name
            if pack_path != renamed_pack && !there {
                std::fs::rename(&renamed_pack, pack_path)?;
            }
            return Err(e);
        }
        Ok(())
    })();
    if let Err(e) = moved {
        if idx_path != renamed_idx {
            let _ = std::fs::remove_file(&idx_path);
        }
        return Err(e.into());
    }
    Ok((renamed_pack, renamed_idx))
}

fn write_idx(path: &Path, idx: &[u8], kept: bool) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(idx)?;
    if kept {
        f.sync_all()?;
    }
    Ok(())
}

fn verify_trailer(map: &[u8], algo: HashAlgo, trailer: &[u8]) -> Result<(), PackError> {
    use alt_hash::{Sha1, Sha256};
    let body = &map[..map.len() - trailer.len()];
    let actual: Vec<u8> = match algo {
        HashAlgo::Sha1 => {
            let mut h = Sha1::new();
            h.update(body);
            h.finalize().to_vec()
        }
        HashAlgo::Sha256 => {
            let mut h = Sha256::new();
            h.update(body);
            h.finalize().to_vec()
        }
    };
    if actual != trailer {
        return Err(PackError::Format("pack trailer mismatch"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IndexedPack, PackWriter};
    use alt_git_codec::ObjectKind;
    use alt_os::temp::TempDir;

    fn put(writer: &mut PackWriter, kind: ObjectKind, data: &[u8]) -> ObjectId {
        let oid = ObjectId::hash_object(HashAlgo::Sha1, kind, data);
        writer.add(oid, kind, data).unwrap();
        oid
    }

    /// A pack written by `PackWriter` (plain entries only) round-trips
    /// through `index_pack` — the produced idx is byte-equivalent to what
    /// `PackWriter::finish` wrote, and `IndexedPack::open` reads back the
    /// same objects.
    #[test]
    fn plain_pack_round_trips_through_index_pack() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path();

        let mut writer = PackWriter::create(pack_dir, HashAlgo::Sha1, 3).unwrap();
        let blob1 = put(&mut writer, ObjectKind::Blob, b"hello\n");
        let blob2 = put(&mut writer, ObjectKind::Blob, b"world\n");
        let tree = put(&mut writer, ObjectKind::Tree, b"");
        let written = writer.finish().unwrap();

        // delete the writer's idx and rebuild it from scratch via index_pack
        std::fs::remove_file(&written.idx_path).unwrap();
        let rebuilt = index_pack(&written.pack_path, HashAlgo::Sha1, false).unwrap();
        assert_eq!(rebuilt.objects, 3);
        assert_eq!(rebuilt.trailer, written.trailer);

        // opening as IndexedPack reads back every object
        let ip = IndexedPack::open(&rebuilt.pack_path, HashAlgo::Sha1).unwrap();
        for oid in [blob1, blob2, tree] {
            let obj = ip
                .read(&oid)
                .unwrap()
                .expect("object present after index_pack");
            // round-trip preserves identity
            let actual = ObjectId::hash_object(HashAlgo::Sha1, obj.kind, &obj.data);
            assert_eq!(actual, oid);
        }
    }

    /// A pack whose trailer doesn't match the body hash is rejected — a
    /// corrupt download must not produce a misleadingly-good idx.
    #[test]
    fn corrupt_trailer_is_rejected() {
        let dir = TempDir::new().unwrap();
        let mut writer = PackWriter::create(dir.path(), HashAlgo::Sha1, 1).unwrap();
        put(&mut writer, ObjectKind::Blob, b"x");
        let written = writer.finish().unwrap();

        // flip a byte in the trailer
        let mut bytes = std::fs::read(&written.pack_path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&written.pack_path, &bytes).unwrap();
        std::fs::remove_file(&written.idx_path).unwrap();

        let err = index_pack(&written.pack_path, HashAlgo::Sha1, false).unwrap_err();
        match err {
            PackError::Format(msg) => assert!(msg.contains("trailer"), "msg={msg}"),
            other => panic!("expected Format trailer error, got {other:?}"),
        }
    }
}
