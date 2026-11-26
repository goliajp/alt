//! The files of a backup: each read as a regular file, with its digest, into
//! the archive under its name.

use super::{Error, FileDigest, Result};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

pub(super) fn regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
        .open(path)
        .map_err(|_| Error::File)?;
    if !file.metadata().map_err(|_| Error::File)?.is_file() {
        return Err(Error::Config("backup entry is not a regular file"));
    }
    Ok(file)
}
pub(super) fn digest(file: &mut (impl Read + Seek)) -> Result<FileDigest> {
    file.rewind().map_err(|_| Error::File)?;
    let mut hash = alt_hash::blake3::Hasher::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|_| Error::File)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        bytes = bytes
            .checked_add(n as u64)
            .ok_or(Error::Config("backup size overflow"))?;
    }
    file.rewind().map_err(|_| Error::File)?;
    Ok(FileDigest {
        bytes,
        blake3: hash.finalize().to_hex().to_string(),
    })
}
pub(super) fn append<W: Write>(
    archive: &mut alt_tar::Builder<W>,
    name: &str,
    file: &mut (impl Read + Seek),
) -> Result<FileDigest> {
    let description = digest(file)?;
    archive
        .append_file(name.as_bytes(), 0o600, 0, description.bytes, file)
        .map_err(|_| Error::File)?;
    Ok(description)
}
pub(super) fn blob_path(blob: &alt_server_store::SnapshotBlob) -> String {
    format!("bytes/{}/{}", blob.repository, blob.blake3)
}
pub(super) fn segment_path(segment: &alt_server_store::SnapshotPackSegment) -> String {
    format!("packs/{}/{}", segment.repository, segment.segment)
}

/// Appends the file `name` in `work`, written with `content` first where
/// that is given, to the archive.
pub(super) fn appended<W: Write>(
    archive: &mut alt_tar::Builder<W>,
    (work, name): (&Path, &str),
    content: Option<&[u8]>,
) -> Result<FileDigest> {
    let path = work.join(name);
    if let Some(content) = content {
        crate::operator::create_private(&path, content)?;
    }
    append(archive, name, &mut regular(&path)?)
}
