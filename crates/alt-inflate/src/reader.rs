//! decompression as a `Read` over a compressed `Read`

use crate::{Error, Format, Inflater};
use std::io::{self, Read};

/// Reads the decompressed content of the stream that `source` supplies.
pub struct Reader<R> {
    source: R,
    inflater: Inflater,
    /// compressed bytes read from the source and not consumed yet
    buffer: Vec<u8>,
    start: usize,
    source_ended: bool,
    done: bool,
    /// gzip only: go on with the next member after one ends
    members: bool,
}

impl<R: Read> Reader<R> {
    pub fn new(format: Format, source: R) -> Self {
        Self {
            source,
            inflater: Inflater::new(format),
            buffer: Vec::new(),
            start: 0,
            source_ended: false,
            done: false,
            members: false,
        }
    }

    /// a gzip file of one or more members, read as their concatenated content
    pub fn gzip_members(source: R) -> Self {
        Self {
            members: true,
            ..Self::new(Format::Gzip, source)
        }
    }

    /// compressed bytes consumed so far in the current stream
    pub fn total_in(&self) -> u64 {
        self.inflater.total_in()
    }

    pub fn into_inner(self) -> R {
        self.source
    }

    /// reads more from the source behind what is still unconsumed
    fn refill(&mut self) -> io::Result<()> {
        self.buffer.drain(..self.start);
        self.start = 0;
        let held = self.buffer.len();
        self.buffer.resize(held + (32 << 10), 0);
        let read = self.source.read(&mut self.buffer[held..])?;
        self.buffer.truncate(held + read);
        self.source_ended = read == 0;
        Ok(())
    }
}

impl<R: Read> Read for Reader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            if self.done {
                if !self.members {
                    return Ok(0);
                }
                // another member, or the clean end of the file
                if self.start == self.buffer.len() {
                    self.refill()?;
                    if self.buffer.is_empty() {
                        return Ok(0);
                    }
                }
                self.inflater.reset();
                self.done = false;
            }
            let step = self
                .inflater
                .inflate(&self.buffer[self.start..], output)
                .map_err(io::Error::from)?;
            self.start += step.consumed;
            self.done = step.done;
            if step.produced != 0 || (step.done && !self.members) {
                return Ok(step.produced);
            }
            if !step.done {
                if self.source_ended {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        Error::Truncated,
                    ));
                }
                self.refill()?;
            }
        }
    }
}
