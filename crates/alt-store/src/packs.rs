//! The pack files of a chunk store on disk: listing them, opening the
//! active one for append (crash recovery included), and bringing the chunk
//! index up to date with whatever its runs do not cover yet.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use alt_os::map::Mmap;

use crate::index::{ChunkIndex, Location};
use crate::pack::{self, REC_HEADER_LEN};
use crate::runs::{GUARD_LEN, Guard};
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

/// The 16 bytes before `loc` in its pack, when the pack reaches that far.
pub(crate) fn guard_at(dir: &Path, loc: Location) -> Option<Guard> {
    if loc.offset < (pack::HEADER_LEN + REC_HEADER_LEN) as u64 {
        return None;
    }
    let file = File::open(pack_path(dir, loc.seq)).ok()?;
    if file.metadata().ok()?.len() < loc.offset {
        return None;
    }
    let mut guard = [0u8; GUARD_LEN];
    pack::read_exact_at(&file, &mut guard, loc.offset - GUARD_LEN as u64).ok()?;
    Some(guard)
}

/// Opens (or creates) the active pack: validates the header, walks the
/// records from offset `from` (where the index runs stop, or the first
/// record), and truncates any torn tail so the writer starts at a clean
/// offset. Returns the pack and the records walked.
pub(crate) fn open_active(
    path: &Path,
    create: bool,
    from: u64,
) -> Result<(Active, Vec<(pack::RecordHeader, u64)>), StoreError> {
    if create {
        // create-new can lose a race to another process bringing the same
        // fresh store up; if it already exists, fall through and open it.
        match OpenOptions::new().create_new(true).write(true).open(path) {
            Ok(mut write) => {
                write.write_all(&pack::file_header())?;
                write.sync_all()?;
                let read = File::open(path)?;
                let active = Active {
                    seq: 0, // caller fills in
                    write,
                    read,
                    len: pack::HEADER_LEN as u64,
                };
                return Ok((active, Vec::new()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }

    let read = File::open(path)?;
    let size = read.metadata()?.len();
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

    let mut write = OpenOptions::new().write(true).open(path)?;
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
    write.seek(SeekFrom::Start(valid_len))?;
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
pub(crate) type PackState = (HashMap<u32, OnceLock<Mmap>>, Active, ChunkIndex);

/// Maps a sealed pack and checks its header.
pub(crate) fn map_sealed(dir: &Path, seq: u32) -> Result<Mmap, StoreError> {
    let file = File::open(pack_path(dir, seq))?;
    let map = unsafe { Mmap::map(&file)? };
    pack::check_file_header(&map)?;
    Ok(map)
}

/// Loads the on-disk pack set: the index runs, then every record they do
/// not cover — the rest of the pack they stop in and all later packs.
pub(crate) fn load(dir: &Path) -> Result<PackState, StoreError> {
    std::fs::create_dir_all(dir)?;
    let seqs = list_pack_seqs(dir)?;
    let mut sealed = HashMap::new();
    let first = Location {
        seq: seqs.first().copied().unwrap_or(1),
        offset: pack::HEADER_LEN as u64,
    };
    let mut index = ChunkIndex::open(dir, first, |loc| guard_at(dir, loc))?;

    let (&active_seq, sealed_seqs) = match seqs.split_last() {
        Some((last, rest)) => (last, rest),
        None => {
            let (mut active, _) = open_active(&pack_path(dir, 1), true, first.offset)?;
            active.seq = 1;
            pack::fsync_dir(dir)?;
            return Ok((sealed, active, index));
        }
    };
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

    let (mut active, recs) = open_active(&pack_path(dir, active_seq), false, from(active_seq))?;
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
