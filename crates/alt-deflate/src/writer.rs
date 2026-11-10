//! compression behind `Write`

use crate::{Format, compress};
use std::io::{self, Write};

/// Collects what is written and compresses it as one stream when finished: the whole
/// input is what the compressor works on.
pub struct Writer<W: Write> {
    sink: W,
    data: Vec<u8>,
    level: u8,
    format: Format,
}

impl<W: Write> Writer<W> {
    pub fn new(sink: W, level: u8, format: Format) -> Self {
        Self {
            sink,
            data: Vec::new(),
            level,
            format,
        }
    }

    /// bytes written so far, uncompressed
    pub fn total_in(&self) -> u64 {
        self.data.len() as u64
    }

    /// compresses everything written, writes it to the sink and returns the sink
    pub fn finish(mut self) -> io::Result<W> {
        self.sink
            .write_all(&compress(&self.data, self.level, self.format))?;
        Ok(self.sink)
    }
}

impl<W: Write> Write for Writer<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.data.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
