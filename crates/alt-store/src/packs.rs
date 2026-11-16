//! The pack files of a chunk store on disk: listing them, opening the
//! active one for append (crash recovery included), and bringing the chunk
//! index up to date with whatever its runs do not cover yet.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use alt_os::map::Mmap;

use crate::index::{ChunkIndex, Location};
use crate::pack::{self, REC_HEADER_LEN};
use crate::runs::{GUARD_SPAN, Guard};
use crate::{StoreError, Tail, idx};

pub(crate) struct Active {
    pub seq: u32,
    /// Cursor sits at `len`; appends only ever move it forward.
    pub write: File,
    /// Separate handle for positioned reads so the cursor stays the writer's.
    pub read: File,
    pub len: u64,
}

pub(crate) fn pack_path(dir: &Path, seq: u32) -> PathBuf {
    dir.join(format!("pack-{seq:08}.altpack"))
}

pub(crate) fn idx_path(dir: &Path, seq: u32) -> PathBuf {
    dir.join(format!("pack-{seq:08}.altidx"))
}

pub(crate) fn list_pack_seqs(dir: &Path) -> Result<Vec<u32>, StoreError> {
    let mut seqs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(seq) = name
            .strip_prefix("pack-")
            .and_then(|s| s.strip_suffix(".altpack"))
            .and_then(|s| s.parse::<u32>().ok())
        {
            seqs.push(seq);
        }
    }
    seqs.sort_unstable();
    Ok(seqs)
}

/// The bytes before `loc` in its pack that guard an index run ending there,
/// when the pack reaches that far.
pub(crate) fn guard_at(dir: &Path, loc: Location) -> Option<Guard> {
    let file = File::open(pack_path(dir, loc.seq)).ok()?;
    let size = file.metadata().ok()?.len();
    guard_in(&file, size, loc.offset)
}

/// The same in an open pack of `size` bytes.
fn guard_in(pack: &File, size: u64, offset: u64) -> Option<Guard> {
    if offset < (pack::HEADER_LEN + REC_HEADER_LEN) as u64 || size < offset {
        return None;
    }
    let span = offset.min(GUARD_SPAN as u64);
    let mut guard = vec![0u8; span as usize];
    pack::read_exact_at(pack, &mut guard, offset - span).ok()?;
    Some(guard)
}

/// Creates active pack `seq` with its header. Another process bringing the
/// same fresh store up may have created it first; it is then opened as is.
pub(crate) fn create_active(dir: &Path, seq: u32, tail: &Tail) -> Result<Active, StoreError> {
    let path = pack_path(dir, seq);
    match OpenOptions::new().create_new(true).append(true).open(&path) {
        Ok(mut write) => {
            write.write_all(&pack::file_header())?;
            write.sync_all()?;
            Ok(Active {
                seq,
                write,
                read: File::open(&path)?,
                len: pack::HEADER_LEN as u64,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let read = File::open(&path)?;
            let size = read.metadata()?.len();
            let walk = Walk {
                seq,
                from: pack::HEADER_LEN as u64,
                whole: false,
                tail,
            };
            Ok(open_active(&path, read, size, walk)?.0)
        }
        Err(e) => Err(e.into()),
    }
}

/// How [`open_active`] reads the active pack.
pub(crate) struct Walk<'a> {
    pub seq: u32,
    /// where the index runs stop in the pack, or its first record
    pub from: u64,
    /// every record is known to have been written whole: a walk that stops
    /// short is damage whatever `tail` says
    pub whole: bool,
    pub tail: &'a Tail,
}

/// Checks the header of a pack of `size` bytes and walks its records from
/// offset `from`: the records, and where the walk stopped (zero in a pack
/// cut short inside its header).
fn walked(
    read: &File,
    size: u64,
    from: u64,
) -> Result<(Vec<(pack::RecordHeader, u64)>, u64), StoreError> {
    let mut head = [0u8; pack::HEADER_LEN];
    let have = (size as usize).min(pack::HEADER_LEN);
    pack::read_exact_at(read, &mut head[..have], 0)?;
    if have < pack::HEADER_LEN {
        // a crash between create and the header fsync can leave a short
        // file; anything that is not a header prefix is foreign
        if !pack::file_header().starts_with(&head[..have]) {
            return Err(StoreError::Format("bad altpack header"));
        }
        return Ok((Vec::new(), 0));
    }
    pack::check_file_header(&head)?;
    if size == from {
        return Ok((Vec::new(), size));
    }
    let map = unsafe { Mmap::map(read)? };
    Ok(pack::scan_from(&map, from as usize))
}

/// Opens the active pack through its read handle `read` (`size` bytes
/// long): validates the header and walks the records. A walk that stops
/// short of the end stopped at an unfinished append, which is cut off, or
/// at damage, which is reported with the pack left as it is (see [`Tail`]).
/// Returns the records walked.
pub(crate) fn open_active(
    path: &Path,
    read: File,
    size: u64,
    walk: Walk,
) -> Result<(Active, Vec<(pack::RecordHeader, u64)>), StoreError> {
    let (recs, valid_len) = walked(&read, size, walk.from)?;
    let unfinished = valid_len == 0 || walk.tail.unfinished(walk.seq, valid_len);
    if valid_len != size && (walk.whole || !unfinished) {
        return Err(StoreError::Format("active pack damaged"));
    }
    let len = valid_len.max(pack::HEADER_LEN as u64);
    let write = if walk.tail.is_read_only() {
        read.try_clone()?
    } else {
        // append mode: writes land at the end wherever a truncation moved it
        let mut write = OpenOptions::new().append(true).open(path)?;
        if valid_len == 0 {
            write.set_len(0)?;
            write.write_all(&pack::file_header())?;
            write.sync_all()?;
        } else if valid_len < size {
            write.set_len(valid_len)?;
            write.sync_all()?;
        }
        write
    };
    let active = Active {
        seq: walk.seq,
        write,
        read,
        len,
    };
    Ok((active, recs))
}

/// The in-memory pack state a chunk store holds: sealed packs (mapped on
/// first read), the active pack (open for append), and the chunk index.
pub(crate) type PackState = (BTreeMap<u32, OnceLock<Mmap>>, Active, ChunkIndex);

/// Maps a sealed pack and checks its header.
pub(crate) fn map_sealed(dir: &Path, seq: u32) -> Result<Mmap, StoreError> {
    let file = File::open(pack_path(dir, seq))?;
    let map = unsafe { Mmap::map(&file)? };
    pack::check_file_header(&map)?;
    Ok(map)
}

/// What an opener can be told about a store's files, so that it opens them
/// by name instead of reading directories. It is advice: each part is
/// checked against the files, and a part that does not hold (the store
/// changed since it was recorded) is found out by reading the directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Known {
    /// pack sequence numbers, ascending; the last one is the active pack
    pub packs: Option<Vec<u32>>,
    /// the chunk index's runs (see [`crate::runs::Runs::open_named`])
    pub chunk_runs: Option<Vec<(u64, u64)>>,
    /// the blob map's runs
    pub map_runs: Option<Vec<(u64, u64)>>,
}

/// Loads the on-disk pack set: the index runs, then every record they do
/// not cover — the rest of the pack they stop in and all later packs.
pub(crate) fn load(
    dir: &Path,
    known: Option<&Known>,
    tail: &Tail,
) -> Result<PackState, StoreError> {
    match load_from(dir, known, false, tail) {
        // a named pack that is not there: the names are out of date
        Err(StoreError::Io(e)) if known.is_some() && e.kind() == std::io::ErrorKind::NotFound => {
            load_from(dir, None, false, tail)
        }
        loaded => loaded,
    }
}

/// The store's pack sequence numbers: the named ones while no pack follows
/// them, the directory's otherwise. A missing directory is created, except
/// by a read-only open.
fn pack_seqs(dir: &Path, known: Option<&Known>, read_only: bool) -> Result<Vec<u32>, StoreError> {
    let named = known
        .and_then(|known| known.packs.as_deref())
        // a pack after the last one named was added since they were recorded
        .filter(|seqs| {
            seqs.last()
                .is_some_and(|last| !pack_path(dir, last + 1).exists())
        });
    match named {
        Some(seqs) => Ok(seqs.to_vec()),
        None => match list_pack_seqs(dir) {
            Err(StoreError::Io(e)) if !read_only && e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(dir)?;
                Ok(Vec::new())
            }
            listed => listed,
        },
    }
}

/// The state of a store with no pack yet: pack 1 is created, except by a
/// read-only open, which finds no store there.
fn first_pack(
    dir: &Path,
    first: Location,
    runs: Option<&[(u64, u64)]>,
    tail: &Tail,
) -> Result<PackState, StoreError> {
    if tail.is_read_only() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound).into());
    }
    let index = ChunkIndex::open(dir, first, runs, |loc| guard_at(dir, loc))?;
    let active = create_active(dir, 1, tail)?;
    pack::fsync_dir(dir)?;
    Ok((BTreeMap::new(), active, index))
}

/// `whole`: see [`Walk`].
pub(crate) fn load_from(
    dir: &Path,
    known: Option<&Known>,
    whole: bool,
    tail: &Tail,
) -> Result<PackState, StoreError> {
    let seqs = pack_seqs(dir, known, tail.is_read_only())?;
    let runs = known.and_then(|known| known.chunk_runs.as_deref());
    let first = Location {
        seq: seqs.first().copied().unwrap_or(1),
        offset: pack::HEADER_LEN as u64,
    };
    let Some((&active_seq, sealed_seqs)) = seqs.split_last() else {
        return first_pack(dir, first, runs, tail);
    };
    // the index runs usually stop in the active pack: its one read handle
    // serves their guard check too
    let read = File::open(pack_path(dir, active_seq))?;
    let size = read.metadata()?.len();
    let mut index = ChunkIndex::open(dir, first, runs, |loc| {
        if loc.seq == active_seq {
            guard_in(&read, size, loc.offset)
        } else {
            guard_at(dir, loc)
        }
    })?;
    let covered = index.covered();
    let mut sealed = BTreeMap::new();
    for &seq in sealed_seqs {
        let map = OnceLock::new();
        if seq >= covered.seq {
            let from = covered.offset_in(seq);
            let _ = map.set(index_sealed(dir, seq, from, &mut index, tail)?);
        }
        sealed.insert(seq, map);
    }
    let (seq, from) = (active_seq, covered.offset_in(active_seq));
    let walk = Walk {
        seq,
        from,
        whole,
        tail,
    };
    let (active, recs) = open_active(&pack_path(dir, seq), read, size, walk)?;
    for (hdr, offset) in recs {
        let end = offset + (REC_HEADER_LEN + hdr.stored_len as usize) as u64;
        index.insert(hdr.id, Location { seq, offset }, end);
    }
    Ok((sealed, active, index))
}

/// Feeds the records of sealed pack `seq` from offset `from` on into the
/// index, from the pack's idx when it lists the whole pack, and maps it.
fn index_sealed(
    dir: &Path,
    seq: u32,
    from: u64,
    index: &mut ChunkIndex,
    tail: &Tail,
) -> Result<Mmap, StoreError> {
    let map = map_sealed(dir, seq)?;
    let whole = from == pack::HEADER_LEN as u64;
    let listed = if whole {
        idx::read(&idx_path(dir, seq)).ok()
    } else {
        None
    };
    let mut entries = match listed {
        Some(entries) => entries,
        None => {
            // idx is a cache: rebuild from the pack and repair it
            let (recs, valid_len) = pack::scan_from(&map, from as usize);
            if valid_len != map.len() as u64 {
                return Err(StoreError::Format("sealed pack truncated"));
            }
            let entries: Vec<_> = recs.iter().map(|(hdr, off)| (hdr.id, *off)).collect();
            if whole && !tail.is_read_only() {
                idx::write(&idx_path(dir, seq), &entries)?;
                pack::fsync_dir(dir)?;
            }
            entries
        }
    };
    entries.sort_unstable_by_key(|(_, offset)| *offset);
    for (id, offset) in entries {
        index.insert(id, Location { seq, offset }, map.len() as u64);
    }
    Ok(map)
}
