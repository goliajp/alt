use crate::header::{self, BLOCK};
use crate::{BUFFER, Error, Result, read_some};
use std::io::{self, Read, Write};

/// Writes regular-file members to a stream that need not be seekable.
pub struct Builder<W> {
    writer: W,
    buffer: Vec<u8>,
}

// the header keeps a prefix of a long name, cut at a character boundary
fn stored_name(path: &[u8]) -> &[u8] {
    let Some(head) = path.get(..header::NAME.end) else {
        return path;
    };
    match std::str::from_utf8(head) {
        Ok(_) => head,
        Err(error) => &head[..error.valid_up_to()],
    }
}

impl<W: Write> Builder<W> {
    pub fn new(writer: W) -> Self {
        Builder {
            writer,
            buffer: vec![0; BUFFER],
        }
    }

    /// Appends one regular file. `data` must yield exactly `size` bytes.
    pub fn append_file<R: Read + ?Sized>(
        &mut self,
        path: &[u8],
        mode: u32,
        mtime: u64,
        size: u64,
        data: &mut R,
    ) -> Result<()> {
        if path.len() > header::NAME.end {
            let length = path.len() as u64 + 1;
            self.writer.write_all(&header::long_name(length))?;
            self.writer.write_all(path)?;
            self.pad(header::padding(length) + 1)?;
        }
        self.writer
            .write_all(&header::file(stored_name(path), mode, mtime, size))?;
        let mut left = size;
        while left > 0 {
            let step = left.min(self.buffer.len() as u64) as usize;
            let n = read_some(data, &mut self.buffer[..step])?;
            if n == 0 {
                return Err(Error::Size);
            }
            self.writer.write_all(&self.buffer[..n])?;
            left -= n as u64;
        }
        if read_some(data, &mut self.buffer[..1])? != 0 {
            return Err(Error::Size);
        }
        self.pad(header::padding(size))?;
        Ok(())
    }

    /// Writes the end-of-archive marker and returns the stream.
    pub fn finish(mut self) -> io::Result<W> {
        self.writer.write_all(&[0; 2 * BLOCK])?;
        Ok(self.writer)
    }

    fn pad(&mut self, count: usize) -> io::Result<()> {
        self.writer.write_all(&[0; BLOCK][..count])
    }
}
