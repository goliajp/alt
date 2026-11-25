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

use super::IndexLimits;
use super::scan::{Entry, Form, Scanned};
use crate::PackError;

/// Where the bases a thin pack leaves out are read: the object of an id,
/// nothing for one the store does not hold.
pub type Bases<'a> = &'a dyn Fn(&ObjectId) -> std::io::Result<Option<(ObjectKind, Vec<u8>)>>;

/// The ids the deltas left unresolved name as their bases and no entry
/// resolved so far is the object of, each once.
fn wanted(scanned: &Scanned, oids: &[Option<ObjectId>]) -> Vec<ObjectId> {
    let mut known: Vec<ObjectId> = oids.iter().flatten().copied().collect();
    known.sort_unstable();
    let unresolved = scanned
        .entries
        .iter()
        .zip(oids)
        .filter(|(_, oid)| oid.is_none());
    let mut wanted: Vec<ObjectId> = unresolved
        .filter_map(|(entry, _)| match &entry.form {
            Form::Ref(base) if known.binary_search(base).is_err() => Some(*base),
            _ => None,
        })
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
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

/// Appends to the pack at `path` those of the bases its unresolved
/// deltas name that `bases` has, and adds them to `scanned` as the plain
/// entries they are. The new trailer, or nothing where no base was
/// found. What the bases inflate to counts against the limit like the
/// entries the pack came with.
pub(super) fn complete(
    path: &Path,
    algo: HashAlgo,
    scanned: &mut Scanned,
    oids: &mut Vec<Option<ObjectId>>,
    bases: Bases<'_>,
    limits: &IndexLimits,
) -> Result<Option<Vec<u8>>, PackError> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut at = scanned.body_end as u64;
    file.set_len(at)?;
    file.seek(SeekFrom::Start(at))?;
    let came = scanned.entries.len();
    for oid in wanted(scanned, oids) {
        limits.going_on()?;
        let Some((kind, data)) = bases(&oid)? else {
            continue;
        };
        scanned.largest = scanned.largest.max(data.len() as u64);
        scanned.inflated = (scanned.inflated.checked_add(data.len() as u64))
            .filter(|total| *total <= limits.inflated_bytes)
            .ok_or(PackError::Limit("inflated bytes"))?;
        let mut raw = entry_header(kind, data.len());
        let data_at = at + raw.len() as u64;
        raw.extend(alt_deflate::compress(&data, 1, alt_deflate::Format::Zlib));
        file.write_all(&raw)?;
        scanned.entries.push(Entry {
            offset: at,
            data_at,
            size: data.len() as u64,
            form: Form::Plain(kind),
            crc32: alt_hash::crc32(&raw),
        });
        oids.push(Some(oid));
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
    Ok(Some(trailer))
}
