//! The body of a fetch response that is built object by object: framed for Git's side
//! band as it is written, and moved to a temporary file once it outgrows memory.

use crate::Output;
use alt_server_store::Result;
use std::io::{self, Seek, SeekFrom, Write};

/// the most pack bytes one side-band packet carries
const PACKET: usize = 65515;
/// a response larger than this is kept in a file
const IN_MEMORY: usize = 8 << 20;

/// bytes that stay in memory while they are few
pub(crate) enum Spill {
    Memory(Vec<u8>),
    File(alt_os::temp::NamedTempFile, u64),
}

impl Spill {
    pub fn new() -> Self {
        Self::Memory(Vec::new())
    }

    pub fn into_output(self, content_type: &'static str) -> Result<Output> {
        match self {
            Self::Memory(bytes) => Ok(Output::Bytes {
                content_type,
                bytes,
            }),
            Self::File(mut file, bytes) => {
                file.as_file_mut().seek(SeekFrom::Start(0))?;
                Ok(Output::Temporary {
                    content_type,
                    bytes,
                    file,
                })
            }
        }
    }
}

impl Write for Spill {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self {
            Self::Memory(bytes) if bytes.len() + data.len() <= IN_MEMORY => {
                bytes.extend_from_slice(data);
            }
            Self::Memory(bytes) => {
                let mut file = alt_os::temp::NamedTempFile::new()?;
                file.as_file_mut().write_all(bytes)?;
                file.as_file_mut().write_all(data)?;
                let len = (bytes.len() + data.len()) as u64;
                *self = Self::File(file, len);
            }
            Self::File(file, len) => {
                file.as_file_mut().write_all(data)?;
                *len += data.len() as u64;
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// what is written here goes to `inner` as packets on the pack band
pub(crate) struct Band<'a, W: Write> {
    inner: &'a mut W,
    packet: Vec<u8>,
}

impl<'a, W: Write> Band<'a, W> {
    pub fn new(inner: &'a mut W) -> Self {
        Self {
            inner,
            packet: Vec::with_capacity(PACKET),
        }
    }

    fn send(&mut self) -> io::Result<()> {
        if !self.packet.is_empty() {
            self.inner
                .write_all(format!("{:04x}", self.packet.len() + 5).as_bytes())?;
            self.inner.write_all(&[1])?;
            self.inner.write_all(&self.packet)?;
            self.packet.clear();
        }
        Ok(())
    }

    /// sends what is left of the last packet
    pub fn finish(mut self) -> io::Result<()> {
        self.send()
    }
}

impl<W: Write> Write for Band<'_, W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let take = data.len().min(PACKET - self.packet.len());
        self.packet.extend_from_slice(&data[..take]);
        if self.packet.len() == PACKET {
            self.send()?;
        }
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn packets_are_full_except_the_last_and_large_bodies_move_to_a_file() {
        for len in [
            0usize,
            1,
            PACKET - 1,
            PACKET,
            PACKET + 1,
            3 * PACKET + 17,
            IN_MEMORY + 5,
        ] {
            let data: Vec<u8> = (0..len).map(|n| (n % 251) as u8).collect();
            let mut body = Spill::new();
            let mut band = Band::new(&mut body);
            // written in uneven pieces, as a pack writer does
            for piece in data.chunks(70_001) {
                band.write_all(piece).unwrap();
            }
            band.finish().unwrap();
            let framed = match body.into_output("test").unwrap() {
                Output::Bytes { bytes, .. } => {
                    assert!(len < IN_MEMORY);
                    bytes
                }
                Output::Temporary {
                    bytes, mut file, ..
                } => {
                    assert!(len > IN_MEMORY);
                    let mut read = Vec::new();
                    file.as_file_mut().read_to_end(&mut read).unwrap();
                    assert_eq!(read.len() as u64, bytes);
                    read
                }
                _ => panic!("unexpected output"),
            };
            let mut rest = framed.as_slice();
            let mut unframed = Vec::new();
            while !rest.is_empty() {
                let size =
                    usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
                assert_eq!(rest[4], 1);
                // only the last packet may be short
                assert!(size == PACKET + 5 || size == rest.len());
                unframed.extend_from_slice(&rest[5..size]);
                rest = &rest[size..];
            }
            assert_eq!(unframed, data);
        }
    }
}
