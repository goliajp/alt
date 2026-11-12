use crate::header::{self, BLOCK, Block};
use crate::{BUFFER, Error, MAX_EXTENSION, MAX_PATH, Result, pax, read_some};
use std::io::{self, Read, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    HardLink,
    SymbolicLink,
    Directory,
    Other(u8),
}

impl Kind {
    fn from_flag(flag: u8) -> Self {
        match flag {
            0 | b'0' => Kind::File,
            b'1' => Kind::HardLink,
            b'2' => Kind::SymbolicLink,
            b'5' => Kind::Directory,
            other => Kind::Other(other),
        }
    }
}

/// Reads members in order from a stream that need not be seekable.
pub struct Archive<R> {
    reader: R,
    // data and padding of the current member not yet consumed
    data: u64,
    padding: usize,
    buffer: Vec<u8>,
}

pub struct Entry<'a, R> {
    archive: &'a mut Archive<R>,
    path: Vec<u8>,
    kind: Kind,
    size: u64,
}

fn fill<R: Read>(reader: &mut R, mut buffer: &mut [u8]) -> Result<()> {
    while !buffer.is_empty() {
        match read_some(reader, buffer)? {
            0 => return Err(Error::Truncated),
            n => buffer = &mut buffer[n..],
        }
    }
    Ok(())
}

fn checked_path(path: Vec<u8>) -> Result<Vec<u8>> {
    if path.len() > MAX_PATH {
        return Err(Error::TooLong);
    }
    if path.is_empty()
        || path[0] == b'/'
        || path.contains(&0)
        || path.split(|&b| b == b'/').any(|part| part == b"..")
    {
        return Err(Error::Path);
    }
    Ok(path)
}

impl<R: Read> Archive<R> {
    pub fn new(reader: R) -> Self {
        Archive {
            reader,
            data: 0,
            padding: 0,
            buffer: vec![0; BUFFER],
        }
    }

    /// The stream, positioned wherever reading stopped. After the last member
    /// that is just past the first all-zero block.
    pub fn into_inner(self) -> R {
        self.reader
    }

    /// The next member, skipping whatever is left of the previous one.
    /// `None` at the end-of-archive marker or at a clean end of the stream.
    pub fn next_entry(&mut self) -> Result<Option<Entry<'_, R>>> {
        self.skip(self.data)?;
        self.skip(self.padding as u64)?;
        (self.data, self.padding) = (0, 0);
        let mut long_name = None;
        let mut extensions = None;
        loop {
            let Some(block) = self.header()? else {
                if long_name.is_some() || extensions.is_some() {
                    return Err(Error::Truncated);
                }
                return Ok(None);
            };
            let size = header::size(&block)?;
            match block[header::KIND] {
                b'L' if long_name.is_some() => return Err(Error::Extension),
                b'L' => {
                    let mut name = self.extension(size)?;
                    name.truncate(name.iter().position(|&b| b == 0).unwrap_or(name.len()));
                    long_name = Some(name);
                }
                b'x' if extensions.is_some() => return Err(Error::Extension),
                b'x' => extensions = Some(pax::parse(&self.extension(size)?)?),
                b'K' | b'g' => drop(self.extension(size)?),
                b'S' => return Err(Error::Unsupported),
                flag => {
                    let extensions = extensions.unwrap_or_default();
                    let path = long_name
                        .or(extensions.path)
                        .unwrap_or_else(|| header::path(&block));
                    let size = extensions.size.unwrap_or(size);
                    self.data = size;
                    self.padding = header::padding(size);
                    return Ok(Some(Entry {
                        path: checked_path(path)?,
                        kind: Kind::from_flag(flag),
                        size,
                        archive: self,
                    }));
                }
            }
        }
    }

    fn header(&mut self) -> Result<Option<Block>> {
        let mut block = [0; BLOCK];
        let mut filled = 0;
        while filled < BLOCK {
            match read_some(&mut self.reader, &mut block[filled..])? {
                0 if filled == 0 => return Ok(None),
                0 => return Err(Error::Truncated),
                n => filled += n,
            }
        }
        if block.iter().all(|&b| b == 0) {
            return Ok(None);
        }
        header::verify(&block)?;
        Ok(Some(block))
    }

    fn extension(&mut self, size: u64) -> Result<Vec<u8>> {
        if size > MAX_EXTENSION {
            return Err(Error::TooLong);
        }
        let mut data = vec![0; size as usize];
        fill(&mut self.reader, &mut data)?;
        self.skip(header::padding(size) as u64)?;
        Ok(data)
    }

    fn skip(&mut self, mut count: u64) -> Result<()> {
        while count > 0 {
            let step = count.min(self.buffer.len() as u64) as usize;
            fill(&mut self.reader, &mut self.buffer[..step])?;
            count -= step as u64;
        }
        Ok(())
    }
}

impl<R: Read> Entry<'_, R> {
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Writes the rest of this member's data to `writer` and returns its length.
    pub fn copy_to<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<u64> {
        let archive = &mut *self.archive;
        let mut copied = 0;
        while archive.data > 0 {
            let step = archive.data.min(archive.buffer.len() as u64) as usize;
            let n = read_some(&mut archive.reader, &mut archive.buffer[..step])?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            writer.write_all(&archive.buffer[..n])?;
            archive.data -= n as u64;
            copied += n as u64;
        }
        Ok(copied)
    }
}

impl<R: Read> Read for Entry<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let step = self.archive.data.min(buffer.len() as u64) as usize;
        if step == 0 {
            return Ok(0);
        }
        let n = self.archive.reader.read(&mut buffer[..step])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.archive.data -= n as u64;
        Ok(n)
    }
}
