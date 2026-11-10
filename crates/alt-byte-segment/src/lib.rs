//! A bounded, self-describing container of unchanged canonical bytes.
//!
//! This crate knows no paths, repositories, permissions, SQL, or durability.
//! Encoding checks each input, but the caller must synchronize and install the
//! file before publishing any member. Opening checks the index and extent;
//! `verify` additionally checks every payload and the complete checksum.

use std::io::{self, Read, Seek, SeekFrom, Write};

const MAGIC: &[u8; 8] = b"ALTBYT01";
const HEADER: u64 = 64;
const ENTRY: u64 = 48;
const TRAILER: u64 = 32;
pub const MAX_MEMBERS: usize = 8192;
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("segment I/O")]
    Io(#[from] io::Error),
    #[error("segment format: {0}")]
    Format(&'static str),
    #[error("segment limit: {0}")]
    Limit(&'static str),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub members: usize,
    pub bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            members: MAX_MEMBERS,
            bytes: MAX_BYTES,
        }
    }
}
impl Limits {
    fn check(self, members: usize, bytes: u64) -> Result<()> {
        if self.members == 0
            || self.members > MAX_MEMBERS
            || self.bytes > MAX_BYTES
            || members == 0
            || members > self.members
            || bytes > self.bytes
        {
            return Err(Error::Limit("member count or original bytes"));
        }
        Ok(())
    }
}

pub struct Input<'a> {
    pub digest: [u8; 32],
    pub bytes: u64,
    pub source: &'a mut dyn Read,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    digest: [u8; 32],
    offset: u64,
    bytes: u64,
}
impl Member {
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    members: Vec<Member>,
    bytes: u64,
    file_bytes: u64,
    digest: [u8; 32],
}
impl Index {
    pub fn members(&self) -> &[Member] {
        &self.members
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn file_bytes(&self) -> u64 {
        self.file_bytes
    }
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
    pub fn find(&self, digest: &[u8; 32]) -> Option<&Member> {
        self.members
            .binary_search_by_key(digest, |member| member.digest)
            .ok()
            .map(|i| &self.members[i])
    }
}

/// Successful encoding, not a file installation or durability proof.
pub struct Written {
    pub index: Index,
    pub checksum: [u8; 32],
}

/// Write from position zero of a fresh output. Inputs are ordered by digest;
/// duplicates are rejected, and every input must end at its declared size.
pub fn encode(
    output: &mut impl Write,
    inputs: &mut [Input<'_>],
    limits: Limits,
) -> Result<Written> {
    limits.check(inputs.len(), 0)?;
    let bytes = inputs.iter().try_fold(0u64, |sum, item| {
        sum.checked_add(item.bytes)
            .ok_or(Error::Limit("original byte sum"))
    })?;
    limits.check(inputs.len(), bytes)?;
    let mut order: Vec<_> = (0..inputs.len()).collect();
    order.sort_unstable_by_key(|&i| inputs[i].digest);
    let mut offset = HEADER + ENTRY * inputs.len() as u64;
    let mut members = Vec::with_capacity(inputs.len());
    let mut encoded = Vec::with_capacity(inputs.len() * ENTRY as usize);
    for &i in &order {
        let item = &inputs[i];
        if members
            .last()
            .is_some_and(|last: &Member| last.digest == item.digest)
        {
            return Err(Error::Format("duplicate digest"));
        }
        members.push(Member {
            digest: item.digest,
            offset,
            bytes: item.bytes,
        });
        encoded.extend_from_slice(&item.digest);
        encoded.extend_from_slice(&offset.to_le_bytes());
        encoded.extend_from_slice(&item.bytes.to_le_bytes());
        offset += item.bytes;
    }
    let index = Index {
        members,
        bytes,
        file_bytes: offset + TRAILER,
        digest: *blake3::hash(&encoded).as_bytes(),
    };
    let mut header = [0; HEADER as usize];
    header[..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&(inputs.len() as u32).to_le_bytes());
    header[16..24].copy_from_slice(&(encoded.len() as u64).to_le_bytes());
    header[24..32].copy_from_slice(&bytes.to_le_bytes());
    header[32..64].copy_from_slice(&index.digest);
    let mut all = blake3::Hasher::new();
    for chunk in [&header[..], &encoded] {
        output.write_all(chunk)?;
        all.update(chunk);
    }
    let mut buffer = [0; 65536];
    for i in order {
        let item = &mut inputs[i];
        let mut remaining = item.bytes;
        let mut hash = blake3::Hasher::new();
        while remaining > 0 {
            let cap = remaining.min(buffer.len() as u64) as usize;
            let n = item.source.read(&mut buffer[..cap])?;
            if n == 0 {
                return Err(Error::Format("short input"));
            }
            output.write_all(&buffer[..n])?;
            all.update(&buffer[..n]);
            hash.update(&buffer[..n]);
            remaining -= n as u64;
        }
        if item.source.read(&mut buffer[..1])? != 0 || hash.finalize().as_bytes() != &item.digest {
            return Err(Error::Format("input length or digest"));
        }
    }
    let checksum = *all.finalize().as_bytes();
    output.write_all(&checksum)?;
    Ok(Written { index, checksum })
}

/// An index bound to one seekable source. `open` does not validate payloads.
pub struct Segment<R> {
    source: R,
    index: Index,
    checksum: [u8; 32],
}
impl<R: Read + Seek> Segment<R> {
    pub fn open(mut source: R, limits: Limits) -> Result<Self> {
        let file_bytes = source.seek(SeekFrom::End(0))?;
        source.seek(SeekFrom::Start(0))?;
        let mut header = [0; HEADER as usize];
        source.read_exact(&mut header)?;
        if &header[..8] != MAGIC || header[12..16] != [0; 4] {
            return Err(Error::Format("version or flags"));
        }
        let count = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        let index_bytes = u64::from_le_bytes(header[16..24].try_into().unwrap());
        let bytes = u64::from_le_bytes(header[24..32].try_into().unwrap());
        limits.check(count, bytes)?;
        if index_bytes != ENTRY * count as u64
            || file_bytes != HEADER + index_bytes + bytes + TRAILER
        {
            return Err(Error::Format("segment extent"));
        }
        let mut encoded = vec![0; index_bytes as usize];
        source.read_exact(&mut encoded)?;
        let digest = *blake3::hash(&encoded).as_bytes();
        if header[32..64] != digest {
            return Err(Error::Format("index checksum"));
        }
        let mut members = Vec::with_capacity(count);
        let mut position = HEADER + index_bytes;
        for raw in encoded.as_chunks::<{ ENTRY as usize }>().0 {
            let member = Member {
                digest: raw[..32].try_into().unwrap(),
                offset: u64::from_le_bytes(raw[32..40].try_into().unwrap()),
                bytes: u64::from_le_bytes(raw[40..48].try_into().unwrap()),
            };
            if member.offset != position
                || members
                    .last()
                    .is_some_and(|last: &Member| last.digest >= member.digest)
            {
                return Err(Error::Format("member ordering or position"));
            }
            position = position
                .checked_add(member.bytes)
                .ok_or(Error::Format("member overflow"))?;
            if position > file_bytes - TRAILER {
                return Err(Error::Format("member extent"));
            }
            members.push(member);
        }
        if position != file_bytes - TRAILER {
            return Err(Error::Format("unclaimed payload bytes"));
        }
        source.seek(SeekFrom::Start(position))?;
        let mut checksum = [0; 32];
        source.read_exact(&mut checksum)?;
        Ok(Self {
            source,
            index: Index {
                members,
                bytes,
                file_bytes,
                digest,
            },
            checksum,
        })
    }
    pub fn index(&self) -> &Index {
        &self.index
    }
    /// Recorded trailer. Use `verify` before treating it as verified content.
    pub fn recorded_checksum(&self) -> &[u8; 32] {
        &self.checksum
    }
    pub fn source(&self) -> &R {
        &self.source
    }
    pub fn member(&mut self, digest: &[u8; 32]) -> Result<Option<MemberReader<&mut R>>> {
        let Some(member) = self.index.find(digest).cloned() else {
            return Ok(None);
        };
        MemberReader::new(&mut self.source, member).map(Some)
    }
    pub fn into_member(self, digest: &[u8; 32]) -> Result<Option<MemberReader<R>>> {
        let Some(member) = self.index.find(digest).cloned() else {
            return Ok(None);
        };
        MemberReader::new(self.source, member).map(Some)
    }
    /// Verify full checksum and each member's digest. A valid index alone is
    /// insufficient: a malformed writer can checksum inconsistent payloads.
    pub fn verify(&mut self) -> Result<()> {
        let checksum = self.checksum;
        self.verify_checksum(&checksum)?;
        let mut buffer = [0; 65536];
        for member in &self.index.members {
            let mut reader = MemberReader::new(&mut self.source, member.clone())?;
            let mut hash = blake3::Hasher::new();
            loop {
                let n = reader.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            if hash.finalize().as_bytes() != &member.digest {
                return Err(Error::Format("member checksum"));
            }
        }
        Ok(())
    }
    /// Verify the complete current container against an independently trusted
    /// encoding checksum. This does not establish member identities by itself:
    /// the caller must have obtained `expected` from a validating encoder, or
    /// use `verify` for an untrusted container instead.
    pub fn verify_checksum(&mut self, expected: &[u8; 32]) -> Result<()> {
        if &self.checksum != expected {
            return Err(Error::Format("unexpected segment checksum"));
        }
        if self.source.seek(SeekFrom::End(0))? != self.index.file_bytes {
            return Err(Error::Format("segment extent changed"));
        }
        self.source.seek(SeekFrom::Start(0))?;
        let mut remaining = self.index.file_bytes - TRAILER;
        let mut hash = blake3::Hasher::new();
        let mut buffer = [0; 65536];
        while remaining > 0 {
            let cap = remaining.min(buffer.len() as u64) as usize;
            self.source.read_exact(&mut buffer[..cap])?;
            hash.update(&buffer[..cap]);
            remaining -= cap as u64;
        }
        let mut current = [0; 32];
        self.source.read_exact(&mut current)?;
        if hash.finalize().as_bytes() != expected || &current != expected {
            return Err(Error::Format("segment checksum"));
        }
        Ok(())
    }
}

/// A logical member; reads and seeks cannot enter another member or the index.
pub struct MemberReader<R> {
    source: R,
    member: Member,
    position: u64,
}
impl<R: Read + Seek> MemberReader<R> {
    fn new(mut source: R, member: Member) -> Result<Self> {
        source.seek(SeekFrom::Start(member.offset))?;
        Ok(Self {
            source,
            member,
            position: 0,
        })
    }
    pub fn member(&self) -> &Member {
        &self.member
    }
    pub fn source(&self) -> &R {
        &self.source
    }
}
impl<R: Read> Read for MemberReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let capacity = (self.member.bytes - self.position).min(output.len() as u64) as usize;
        if capacity == 0 {
            return Ok(0);
        }
        let n = self.source.read(&mut output[..capacity])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "segment member truncated",
            ));
        }
        self.position += n as u64;
        Ok(n)
    }
}
impl<R: Seek> Seek for MemberReader<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(v) => i128::from(v),
            SeekFrom::End(v) => i128::from(self.member.bytes) + i128::from(v),
            SeekFrom::Current(v) => i128::from(self.position) + i128::from(v),
        };
        if position < 0 || position > i128::from(self.member.bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside segment member",
            ));
        }
        let position = position as u64;
        self.source
            .seek(SeekFrom::Start(self.member.offset + position))?;
        self.position = position;
        Ok(position)
    }
}
