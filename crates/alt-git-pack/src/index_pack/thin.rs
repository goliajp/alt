//! A thin pack made whole: the bases its deltas name and it leaves out
//! are appended to it, as `git index-pack --fix-thin` appends them.
//!
//! A push is sent thin: a file that changed by a line comes as a delta of
//! the version the receiver has. Such a pack reads as a pack only next
//! to the store it was made for, so the bases are taken out of that
//! store and written behind the last entry, each as a plain entry; the
//! count of the header and the trailer are written anew. From there on
//! the pack is one that holds every base of its deltas.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};

use super::scan::{Entry, Form, Scanned};
use super::{IndexLimits, IndexWork, ObjectMemory, Taken, resolve};
use crate::{Pack, PackError};

/// Where the bases a thin pack leaves out are read: the object of an id,
/// nothing for one the store does not hold.
pub type Bases<'a> = &'a dyn Fn(&ObjectId) -> std::io::Result<Option<(ObjectKind, Vec<u8>)>>;

/// The ids the deltas left unresolved name as their bases and no entry
/// resolved so far is the object of, each once, with the most bytes a
/// delta of it says it has.
fn wanted(scanned: &Scanned, oids: &[Option<ObjectId>]) -> Vec<(ObjectId, u64)> {
    let mut known: Vec<ObjectId> = oids.iter().flatten().copied().collect();
    known.sort_unstable();
    let unresolved = scanned
        .entries
        .iter()
        .zip(oids)
        .filter(|(_, oid)| oid.is_none());
    let mut wanted: Vec<(ObjectId, u64)> = unresolved
        .filter_map(|(entry, _)| match &entry.form {
            Form::Ref(base, size) if known.binary_search(base).is_err() => Some((*base, *size)),
            _ => None,
        })
        .collect();
    // the largest size of each id comes first, and is the one kept
    wanted.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    wanted.dedup_by_key(|(oid, _)| *oid);
    wanted
}

fn entry_header(kind: ObjectKind, bytes: usize) -> Vec<u8> {
    let type_id: u8 = match kind {
        ObjectKind::Commit => 1,
        ObjectKind::Tree => 2,
        ObjectKind::Blob => 3,
        ObjectKind::Tag => 4,
    };
    let mut header = Vec::with_capacity(10);
    let mut size = bytes as u64;
    let mut byte = (type_id << 4) | (size & 0xf) as u8;
    size >>= 4;
    while size > 0 {
        header.push(byte | 0x80);
        byte = (size & 0x7f) as u8;
        size >>= 7;
    }
    header.push(byte);
    header
}

fn digest(file: &mut std::fs::File, algo: HashAlgo) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut piece = vec![0u8; 1 << 20];
    let (mut sha1, mut sha256) = (alt_hash::Sha1::new(), alt_hash::Sha256::new());
    loop {
        let read = file.read(&mut piece)?;
        if read == 0 {
            break;
        }
        match algo {
            HashAlgo::Sha1 => sha1.update(&piece[..read]),
            HashAlgo::Sha256 => sha256.update(&piece[..read]),
        }
    }
    Ok(match algo {
        HashAlgo::Sha1 => sha1.finalize().to_vec(),
        HashAlgo::Sha256 => sha256.finalize().to_vec(),
    })
}

/// Makes the pack at `path` whole, where its unresolved deltas name bases
/// that `bases` has, and resolves the deltas of those. `memory` is what the
/// pack was let in with, as `now` says it has: it is let in again with what
/// it has once the bases are known.
pub(super) fn made_whole(
    (path, algo): (&Path, HashAlgo),
    (scanned, oids): (&mut Scanned, &mut Vec<Option<ObjectId>>),
    bases: Bases<'_>,
    limits: &IndexLimits<'_>,
    (memory, mut now): (&mut Option<Taken<'static>>, (u64, u64)),
    work: &mut IndexWork,
) -> Result<Option<Whole>, PackError> {
    let came = oids.len();
    let wanted = wanted(scanned, oids);
    // the bases are read whole: room for the largest is made before any is
    let largest = wanted.iter().map(|(_, size)| *size).max().unwrap_or(0);
    scanned.largest = scanned.largest.max(largest);
    again(limits, scanned, &mut now, memory)?;
    let objects = limits.objects(memory);
    let (file, wanted) = ((path, algo), (&wanted[..], bases));
    let made = complete(file, scanned, oids, wanted, limits, objects)?;
    let Some(whole) = made else {
        return Ok(None);
    };
    // what the bases inflate to is among what may be held of them
    again(limits, scanned, &mut now, memory)?;
    let pack = Pack::open(path, algo)?;
    let room = limits.objects(memory);
    resolve::resolve(&pack, scanned, oids, came, limits, room, work)?;
    Ok(Some(whole))
}

/// Lets the pack in again with what it has throughout and what its largest
/// object takes as `scanned` says now, where that is not what it has.
fn again(
    limits: &IndexLimits<'_>,
    scanned: &Scanned,
    now: &mut (u64, u64),
    memory: &mut Option<Taken<'static>>,
) -> Result<(), PackError> {
    let after = limits.resolving(scanned);
    if after != *now {
        drop(memory.take());
        *memory = limits.let_in(after.0, after.1)?;
        *now = after;
    }
    Ok(())
}

/// What making a pack whole wrote: the new trailer, and the room on the disk
/// the bases appended to it take.
pub(super) struct Whole {
    pub trailer: Vec<u8>,
    pub appended: Vec<Taken<'static>>,
}

/// Appends to the pack at `path` those of the bases `wanted` that `bases`
/// has, and adds them to `scanned` as the plain entries they are; nothing
/// where no base was found. What the bases inflate to counts against the
/// limit like the entries the pack came with. A base is read where `objects`
/// has room for it, as large as its deltas say it is, and is no other size;
/// it is written where the files of bases have room for it.
fn complete(
    (path, algo): (&Path, HashAlgo),
    scanned: &mut Scanned,
    oids: &mut Vec<Option<ObjectId>>,
    (wanted, bases): (&[(ObjectId, u64)], Bases<'_>),
    limits: &IndexLimits,
    objects: Option<ObjectMemory<'_>>,
) -> Result<Option<Whole>, PackError> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut at = scanned.body_end as u64;
    file.set_len(at)?;
    file.seek(SeekFrom::Start(at))?;
    let came = scanned.entries.len();
    let (mut room, mut appended) = (None, Vec::new());
    for (oid, size) in wanted {
        limits.going_on()?;
        if let Some(objects) = &objects {
            objects.room(&mut room, *size, limits.wait())?;
        }
        let Some((kind, data)) = bases(oid)? else {
            continue;
        };
        if data.len() as u64 != *size {
            return Err(PackError::Format("delta source size mismatch"));
        }
        scanned.inflated = (scanned.inflated.checked_add(*size))
            .filter(|total| *total <= limits.inflated_bytes)
            .ok_or(PackError::Limit("inflated bytes"))?;
        let mut raw = entry_header(kind, data.len());
        let data_at = at + raw.len() as u64;
        raw.extend(alt_deflate::compress(&data, 1, alt_deflate::Format::Zlib));
        drop(data);
        if let Some(files) = limits.files {
            // one that has room already does not wait for what others have
            let wait = appended.is_empty().then(|| limits.wait());
            appended.push(files.append(raw.len() as u64, wait)?);
        }
        file.write_all(&raw)?;
        scanned.entries.push(Entry {
            offset: at,
            data_at,
            size: *size,
            form: Form::Plain(kind),
            crc32: alt_hash::crc32(&raw),
        });
        oids.push(Some(*oid));
        at += raw.len() as u64;
    }
    if scanned.entries.len() == came {
        return Ok(None);
    }
    let count = u32::try_from(scanned.entries.len()).map_err(|_| PackError::Limit("objects"))?;
    file.seek(SeekFrom::Start(8))?;
    file.write_all(&count.to_be_bytes())?;
    let trailer = digest(&mut file, algo)?;
    file.seek(SeekFrom::Start(at))?;
    file.write_all(&trailer)?;
    scanned.body_end = at as usize;
    Ok(Some(Whole { trailer, appended }))
}
