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
use crate::{StoreError, idx};

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

/// Creates the active pack with its header. Another process bringing the
/// same fresh store up may have created it first; it is then opened as is.
pub(crate) fn create_active(path: &Path) -> Result<Active, StoreError> {
    match OpenOptions::new().create_new(true).append(true).open(path) {
        Ok(mut write) => {
            write.write_all(&pack::file_header())?;
            write.sync_all()?;
            Ok(Active {
                seq: 0, // caller fills in
                write,
                read: File::open(path)?,
                len: pack::HEADER_LEN as u64,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let read = File::open(path)?;
            let size = read.metadata()?.len();
            Ok(open_active(path, read, size, pack::HEADER_LEN as u64, false)?.0)
        }
        Err(e) => Err(e.into()),
    }
}

/// Opens the active pack through its read handle `read` (`size` bytes
/// long): validates the header, walks the records from offset `from` (where
/// the index runs stop, or the first record), and cuts off a torn tail.
/// With `whole`, every record is known to have been written whole: a walk
/// that stops short is damage and nothing is cut. Returns the records walked.
pub(crate) fn open_active(
    path: &Path,
    read: File,
    size: u64,
    from: u64,
    whole: bool,
) -> Result<(Active, Vec<(pack::RecordHeader, u64)>), StoreError> {
    let mut head = [0u8; pack::HEADER_LEN];
    let have = (size as usize).min(pack::HEADER_LEN);
    pack::read_exact_at(&read, &mut head[..have], 0)?;
    let (recs, valid_len) = if have < pack::HEADER_LEN {
        // a crash between create and the header fsync can leave a short
        // file; anything that is not a header prefix is foreign
        if !pack::file_header().starts_with(&head[..have]) {
            return Err(StoreError::Format("bad altpack header"));
        }
        (Vec::new(), 0)
    } else {
        pack::check_file_header(&head)?;
        if size == from {
            (Vec::new(), size)
        } else {
            let map = unsafe { Mmap::map(&read)? };
            pack::scan_from(&map, from as usize)
        }
    };

    if whole && valid_len != size {
        return Err(StoreError::Format("active pack damaged"));
    }
    // append mode: writes land at the end wherever a truncation moved it
    let mut write = OpenOptions::new().append(true).open(path)?;
    if valid_len == 0 {
        write.set_len(0)?;
        write.write_all(&pack::file_header())?;
        write.sync_all()?;
        let active = Active {
            seq: 0,
            write,
            read,
            len: pack::HEADER_LEN as u64,
        };
        return Ok((active, Vec::new()));
    }
    if valid_len < size {
        write.set_len(valid_len)?;
        write.sync_all()?;
    }
    let active = Active {
        seq: 0,
        write,
        read,
        len: valid_len,
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
pub(crate) fn load(dir: &Path, known: Option<&Known>) -> Result<PackState, StoreError> {
    match load_from(dir, known, false) {
        // a named pack that is not there: the names are out of date
        Err(StoreError::Io(e)) if known.is_some() && e.kind() == std::io::ErrorKind::NotFound => {
            load_from(dir, None, false)
        }
        loaded => loaded,
    }
}

/// `whole`: see [`open_active`].
pub(crate) fn load_from(
    dir: &Path,
    known: Option<&Known>,
    whole: bool,
) -> Result<PackState, StoreError> {
    let named = known
        .and_then(|known| known.packs.as_deref())
        // a pack after the last one named was added since they were recorded
        .filter(|seqs| {
            seqs.last()
                .is_some_and(|last| !pack_path(dir, last + 1).exists())
        });
    let seqs = match named {
        Some(seqs) => seqs.to_vec(),
        None => match list_pack_seqs(dir) {
            Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(dir)?;
                Vec::new()
            }
            listed => listed?,
        },
    };
    let runs = known.and_then(|known| known.chunk_runs.as_deref());
    let mut sealed = BTreeMap::new();
    let first = Location {
        seq: seqs.first().copied().unwrap_or(1),
        offset: pack::HEADER_LEN as u64,
    };

    let (&active_seq, sealed_seqs) = match seqs.split_last() {
        Some((last, rest)) => (last, rest),
        None => {
            let index = ChunkIndex::open(dir, first, runs, |loc| guard_at(dir, loc))?;
            let mut active = create_active(&pack_path(dir, 1))?;
            active.seq = 1;
            pack::fsync_dir(dir)?;
            return Ok((sealed, active, index));
        }
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
    let from = |seq: u32| {
        if seq == covered.seq {
            covered.offset
        } else {
            pack::HEADER_LEN as u64
        }
    };

    for &seq in sealed_seqs {
        sealed.insert(seq, OnceLock::new());
        if seq < covered.seq {
            continue;
        }
        let map = map_sealed(dir, seq)?;
        let whole = from(seq) == pack::HEADER_LEN as u64;
        let listed = if whole {
            idx::read(&idx_path(dir, seq)).ok()
        } else {
            None
        };
        let mut entries = match listed {
            Some(entries) => entries,
            None => {
                // idx is a cache: rebuild from the pack and repair it
                let (recs, valid_len) = pack::scan_from(&map, from(seq) as usize);
                if valid_len != map.len() as u64 {
                    return Err(StoreError::Format("sealed pack truncated"));
                }
                let entries: Vec<_> = recs.iter().map(|(hdr, off)| (hdr.id, *off)).collect();
                if whole {
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
        let _ = sealed.get(&seq).unwrap().set(map);
    }

    let path = pack_path(dir, active_seq);
    let (mut active, recs) = open_active(&path, read, size, from(active_seq), whole)?;
    active.seq = active_seq;
    for (hdr, offset) in recs {
        let loc = Location {
            seq: active_seq,
            offset,
        };
        let end = offset + (REC_HEADER_LEN + hdr.stored_len as usize) as u64;
        index.insert(hdr.id, loc, end);
    }
    Ok((sealed, active, index))
}
