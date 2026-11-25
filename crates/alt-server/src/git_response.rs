//! The body of a fetch response that is built object by object: framed for Git's side
//! band as it is written, and moved to a temporary file once it outgrows memory.

use crate::Output;
use alt_server_store::{Error, Result};
use std::io::{self, Seek, SeekFrom, Write};

/// the most pack bytes one side-band packet carries
const PACKET: usize = 65515;
/// a response larger than this is kept in a file
const IN_MEMORY: usize = 8 << 20;

/// Bytes that stay in memory while they are few, and are in a file beyond that:
/// one that takes of the room the responses to fetches have on disk, where it is
/// told of that room.
pub(crate) struct Spill {
    held: Held,
    room: Option<alt_shares::Share>,
    /// there was no room for what was written
    full: bool,
}

enum Held {
    Memory(Vec<u8>),
    File(alt_os::temp::NamedTempFile, u64),
}

impl Spill {
    pub fn within(room: Option<alt_shares::Share>) -> Self {
        let (held, full) = (Held::Memory(Vec::new()), false);
        Spill { held, room, full }
    }

    /// What was written, where `written` says all of it was. A response there was
    /// no room for is one the caller asks for again.
    pub fn into_output(self, content_type: &'static str, written: Result<()>) -> Result<Output> {
        if self.full {
            return Err(Error::Busy("responses to fetches"));
        }
        written?;
        match (self.held, self.room) {
            (Held::Memory(bytes), _) => Ok(Output::Bytes {
                content_type,
                bytes,
            }),
            (Held::File(mut file, bytes), room) => {
                file.as_file_mut().seek(SeekFrom::Start(0))?;
                Ok(match room {
                    Some(held) => Output::Sending {
                        content_type,
                        file,
                        held,
                    },
                    None => Output::Temporary {
                        content_type,
                        bytes,
                        file,
                    },
                })
            }
        }
    }

    /// takes `bytes` more of the room, where there is one to take of
    fn take(&mut self, bytes: usize) -> io::Result<()> {
        let taken = self.room.as_mut().map(|room| room.take(bytes as u64));
        if let Some(Err(_)) = taken {
            self.full = true;
            return Err(io::Error::new(io::ErrorKind::StorageFull, "no room"));
        }
        Ok(())
    }
}

impl Write for Spill {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match &mut self.held {
            Held::Memory(bytes) if bytes.len() + data.len() <= IN_MEMORY => {
                bytes.extend_from_slice(data);
            }
            Held::Memory(bytes) => {
                let all = bytes.len() + data.len();
                let bytes = std::mem::take(bytes);
                self.take(all)?;
                let mut file = alt_os::temp::NamedTempFile::new()?;
                file.as_file_mut().write_all(&bytes)?;
                file.as_file_mut().write_all(data)?;
                self.held = Held::File(file, all as u64);
            }
            Held::File(..) => {
                self.take(data.len())?;
                let Held::File(file, len) = &mut self.held else {
                    unreachable!()
                };
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
    fn a_body_that_moves_to_a_file_takes_of_the_room_it_is_given_until_it_is_let_go_of() {
        let room = alt_shares::Shares::of(3 * IN_MEMORY as u64);
        let written = |bytes: usize| {
            let mut body = Spill::within(Some(room.share(None)));
            let written = body.write_all(&vec![7; bytes]);
            body.into_output("test", written.map_err(Error::from))
        };
        // in memory it takes nothing
        assert!(matches!(written(IN_MEMORY), Ok(Output::Bytes { .. })));
        assert_eq!(room.held(), 0);
        let Ok(Output::Sending { held, .. }) = written(2 * IN_MEMORY) else {
            panic!("a body larger than memory holds is a file of the room");
        };
        assert_eq!(
            (held.bytes(), room.held()),
            (2 * IN_MEMORY as u64, 2 * IN_MEMORY as u64)
        );
        // no room for a second beside it: its caller is told to come again
        let refused = written(IN_MEMORY + 1);
        assert!(matches!(refused, Err(Error::Busy(_))));
        assert_eq!(room.held(), 2 * IN_MEMORY as u64);
        drop(held);
        assert!(written(IN_MEMORY + 1).is_ok());
        assert_eq!(room.held(), 0);
    }

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
            let mut body = Spill::within(None);
            let mut band = Band::new(&mut body);
            // written in uneven pieces, as a pack writer does
            for piece in data.chunks(70_001) {
                band.write_all(piece).unwrap();
            }
            band.finish().unwrap();
            let framed = match body.into_output("test", Ok(())).unwrap() {
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
