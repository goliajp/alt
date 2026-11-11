//! Git clients gzip larger upload-pack requests. Both wire and decoded bytes
//! remain bounded, and decoding holds the same worker permit as execution.
use alt_server_store::{Error, Result};
use alt_server_telemetry::Phase;
use std::io::{Read, Seek, SeekFrom, Write};

pub(super) fn decode(
    mut source: alt_os::temp::NamedTempFile,
    max: u64,
) -> Result<alt_os::temp::NamedTempFile> {
    alt_server_store::telemetry::observe(Phase::Decode, || {
        source.as_file_mut().seek(SeekFrom::Start(0))?;
        // Every member's checksum is checked and malformed trailing input is
        // refused. A valid concatenation is one HTTP decoded body.
        let mut decoder = alt_inflate::Reader::gzip_members(source.as_file_mut());
        let mut output = alt_os::temp::NamedTempFile::new()?;
        let mut bytes = 0u64;
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let bound = max.saturating_sub(bytes).saturating_add(1);
            let capacity = buffer
                .len()
                .min(usize::try_from(bound).unwrap_or(usize::MAX));
            let count =
                decoder
                    .read(&mut buffer[..capacity])
                    .map_err(|error| match error.kind() {
                        std::io::ErrorKind::InvalidData
                        | std::io::ErrorKind::InvalidInput
                        | std::io::ErrorKind::UnexpectedEof => {
                            Error::Protocol(alt_remote_protocol::Error::Invalid("Git gzip body"))
                        }
                        _ => Error::Io(error),
                    })?;
            if count == 0 {
                return Ok(output);
            }
            bytes = bytes
                .checked_add(count as u64)
                .filter(|bytes| *bytes <= max)
                .ok_or(Error::Limit("decoded Git request body"))?;
            output.write_all(&buffer[..count])?;
        }
    })
}
