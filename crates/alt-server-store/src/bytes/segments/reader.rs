use super::*;

/// A bounded logical payload. Independent positional reads allow thousands of
/// member proofs/readers to share one descriptor without sharing a seek cursor.
pub struct StoredReader {
    pub(super) file: Arc<File>,
    pub(super) start: u64,
    pub(super) bytes: u64,
    pub(super) position: u64,
}
impl StoredReader {
    /// Bound an independent complete temporary file for transport. Segment
    /// members are constructed only through the checked location readers.
    pub fn from_file(file: File, bytes: u64) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "temporary payload extent",
            ));
        }
        Ok(Self {
            file: Arc::new(file),
            start: 0,
            bytes,
            position: 0,
        })
    }
    /// Transfer a uniquely owned descriptor positioned at this logical cursor.
    /// The descriptor is physically unbounded: transports must enforce `bytes`
    /// separately (for example a fixed-length HTTP body).
    pub fn into_file(self) -> io::Result<File> {
        let mut file = Arc::try_unwrap(self.file)
            .map_err(|_| io::Error::other("shared stored reader cannot transfer its descriptor"))?;
        file.seek(SeekFrom::Start(self.start + self.position))?;
        Ok(file)
    }
    /// Coalesce descriptors only inside one bounded, authorized operation.
    /// Each reader has already opened and verified its current path and range.
    pub(crate) fn share_backing(&mut self, other: &Self) -> Result<()> {
        if Stamp::read(&self.file)? != Stamp::read(&other.file)? {
            return Err(Error::Precondition("segment backing changed"));
        }
        self.file = other.file.clone();
        Ok(())
    }
    pub fn metadata(&self) -> io::Result<fs::Metadata> {
        self.file.metadata()
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}
impl Read for StoredReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let capacity = (self.bytes - self.position).min(output.len() as u64) as usize;
        if capacity == 0 {
            return Ok(0);
        }
        let n = self
            .file
            .read_at(&mut output[..capacity], self.start + self.position)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stored member truncated",
            ));
        }
        self.position += n as u64;
        Ok(n)
    }
}
impl Seek for StoredReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(v) => i128::from(v),
            SeekFrom::Current(v) => i128::from(self.position) + i128::from(v),
            SeekFrom::End(v) => i128::from(self.bytes) + i128::from(v),
        };
        if position < 0 || position > i128::from(self.bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside stored member",
            ));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}
