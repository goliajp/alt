//! The download of an export once what it needs of the database is read: the
//! archive, or one file of it, is written out of the stored bytes and the
//! segments alone. The caller lets go of its store first, so that a download
//! that waits for its turn to be large holds no connection of the database.
use super::ARCHIVE;
use crate::Output;
use crate::git_read::Answer;
use alt_portable::{BlobSource, PackageLimits};
use alt_server_store::{Error, PortableExport, Result};
use std::io::{Read, Seek, SeekFrom, Write};

/// what of the export is downloaded
pub(super) enum Asked {
    Archive,
    Content(String),
}

pub(crate) struct Download {
    export: PortableExport,
    asked: Asked,
    package: PackageLimits,
    /// of the room responses have on disk, for what is written out to be sent
    sending: Option<alt_shares::Share>,
    /// the download counted as being built while the export is held
    _building: Option<alt_shares::Building>,
}

/// What a download takes of the room, about: the bytes of the files it holds.
pub(super) fn bytes(export: &PortableExport, asked: &Asked) -> u64 {
    let blobs = export.envelope().catalog.blobs.iter();
    match asked {
        Asked::Archive => blobs.map(|blob| blob.bytes).sum(),
        Asked::Content(digest) => (blobs.filter(|blob| blob.blake3 == *digest))
            .map(|blob| blob.bytes)
            .sum(),
    }
}

/// The share of a download that was put off, once it has its turn: a download
/// no longer waited for is told to come again.
pub(crate) fn turned(mut share: alt_shares::Share) -> Result<alt_shares::Share> {
    match share.take_turn() {
        Ok(()) => Ok(share),
        Err(_) => Err(Error::Busy("responses to fetches")),
    }
}

impl Download {
    pub(super) fn new(
        export: PortableExport,
        asked: Asked,
        package: PackageLimits,
        sending: Option<alt_shares::Share>,
    ) -> Self {
        let _building = sending.as_ref().map(alt_shares::Share::building);
        Download {
            export,
            asked,
            package,
            sending,
            _building,
        }
    }

    /// What is downloaded, or the share of the room of one put off to wait for
    /// its turn as it was written: the export is let go of, and asked for again
    /// once the turn is had.
    pub(crate) fn answered(self) -> Result<Answer> {
        let Download {
            export,
            asked,
            package,
            sending,
            _building,
        } = self;
        match asked {
            Asked::Archive => {
                let mut body = crate::git_response::Spill::within(sending);
                let envelope = export.envelope();
                let written = alt_portable::write_archive(&mut body, envelope, &export, package);
                body.into_answer(ARCHIVE, written.map_err(Into::into))
            }
            Asked::Content(digest) => content(&export, digest, sending),
        }
    }
}

/// One file of the export, read whole and checked against its digest before
/// it is sent.
fn content(
    export: &PortableExport,
    digest: String,
    sending: Option<alt_shares::Share>,
) -> Result<Answer> {
    let blob = export
        .envelope()
        .catalog
        .blobs
        .iter()
        .find(|b| b.blake3 == digest)
        .ok_or(Error::Unauthorized)?;
    let mut input = export.open(blob)?;
    let mut held = sending;
    if let Some(room) = held.as_mut()
        && room.take(blob.bytes).is_err()
    {
        return match room.put_off() {
            true => Ok(Answer::Turn(held.expect("a room"))),
            false => Err(Error::Busy("responses to fetches")),
        };
    }
    let mut file = alt_os::temp::NamedTempFile::new()?;
    let mut hash = alt_hash::blake3::Hasher::new();
    let mut remaining = blob.bytes;
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let count = input.read(&mut buffer[..remaining.min(65536) as usize])?;
        if count == 0 {
            return Err(Error::Integrity("portable export truncated"));
        }
        hash.update(&buffer[..count]);
        file.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    if input.read(&mut [0])? != 0 || hash.finalize().to_hex().as_str() != blob.blake3 {
        return Err(Error::Integrity("portable export bytes changed"));
    }
    file.as_file_mut().seek(SeekFrom::Start(0))?;
    Ok(Answer::Done(Output::File {
        bytes: blob.bytes,
        digest,
        file: alt_server_store::StoredReader::from_file(file.into_file(), blob.bytes)?,
        held,
    }))
}
