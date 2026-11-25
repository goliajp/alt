//! The download of an export once what it needs of the database is read: the
//! archive, or one file of it, is written out of the stored bytes and the
//! segments alone. The caller lets go of its store first, so that a download
//! that waits for its turn to be large holds no connection of the database.
use super::ARCHIVE;
use crate::Output;
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
}

impl Download {
    pub(super) fn new(
        export: PortableExport,
        asked: Asked,
        package: PackageLimits,
        sending: Option<alt_shares::Share>,
    ) -> Self {
        Download {
            export,
            asked,
            package,
            sending,
        }
    }

    pub(crate) fn answered(self) -> Result<Output> {
        let Download {
            export,
            asked,
            package,
            sending,
        } = self;
        match asked {
            Asked::Archive => {
                let mut body = crate::git_response::Spill::within(sending);
                let envelope = export.envelope();
                let written = alt_portable::write_archive(&mut body, envelope, &export, package);
                body.into_output(ARCHIVE, written.map_err(Into::into))
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
) -> Result<Output> {
    let blob = export
        .envelope()
        .catalog
        .blobs
        .iter()
        .find(|b| b.blake3 == digest)
        .ok_or(Error::Unauthorized)?;
    let mut input = export.open(blob)?;
    let held = match sending {
        Some(mut room) => {
            let full = |_| Error::Busy("responses to fetches");
            room.take(blob.bytes).map_err(full)?;
            Some(room)
        }
        None => None,
    };
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
    Ok(Output::File {
        bytes: blob.bytes,
        digest,
        file: alt_server_store::StoredReader::from_file(file.into_file(), blob.bytes)?,
        held,
    })
}
