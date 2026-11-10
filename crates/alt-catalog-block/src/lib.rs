//! Immutable, bounded directory metadata. This format grants no authority and
//! proves neither payload validity nor durability. Its caller owns repository
//! isolation, current authorization, root transactions and physical storage.
//!
//! A canonical payload may have several roles and Git identities. Blocks are
//! ordered by canonical digest; member sets bind both block identity and count.

mod codec;
mod edit;
mod git_index;
mod members;
pub use edit::{Edit, KeyRange, Page, edit_page};
pub use git_index::{GitIndexBlock, GitIndexEdit, GitIndexEntry, edit_git_page, paginate_git};
pub use members::MemberSet;

use std::collections::{BTreeMap, BTreeSet};

pub type Digest = [u8; 32];
pub const MAX_MEMBERS: usize = 8192;
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const RECORD: u8 = 1 << 4;
pub const ATTACHMENT: u8 = 1 << 5;
pub const TASK_BUNDLE: u8 = 1 << 6;
pub const CHUNK: u8 = 1 << 7;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Format(&'static str),
    Limit(&'static str),
    Conflict(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format(v) => write!(f, "catalog format: {v}"),
            Self::Limit(v) => write!(f, "catalog limit: {v}"),
            Self::Conflict(v) => write!(f, "catalog conflict: {v}"),
        }
    }
}

impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub members: usize,
    pub bytes: usize,
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
    fn validate(self) -> Result<()> {
        if self.members == 0
            || self.members > MAX_MEMBERS
            || self.bytes < codec::OVERHEAD
            || self.bytes > MAX_BYTES
        {
            return Err(Error::Limit("block limits"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum GitKind {
    Commit = 0,
    Tree = 1,
    Blob = 2,
    Tag = 3,
}
impl GitKind {
    pub fn role(self) -> u8 {
        1 << self as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ObjectId {
    Sha1([u8; 20]),
    Sha256([u8; 32]),
}
impl ObjectId {
    /// Lexicographically sortable database key: algorithm followed by raw OID.
    /// Its byte order is identical to `ObjectId::cmp`.
    pub fn key(self) -> Vec<u8> {
        let mut key = vec![self.algorithm()];
        key.extend(self.bytes());
        key
    }
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Sha1(v) => v,
            Self::Sha256(v) => v,
        }
    }
    fn algorithm(self) -> u8 {
        match self {
            Self::Sha1(_) => 1,
            Self::Sha256(_) => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitIdentity {
    pub oid: ObjectId,
    pub kind: GitKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// A selected identity/size with no confirmed stored bytes. It must never
    /// carry a registered Git mapping or be treated as a readable loose file.
    Pending,
    /// Loose-file routing. Availability still depends on caller lifecycle
    /// metadata; a deleting record may outlive the already unlinked file.
    Loose,
    Segment {
        id: [u8; 16],
        index: Digest,
        offset: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub digest: Digest,
    pub bytes: u64,
    /// Git roles occupy bits 0..4; remaining bits are the constants above.
    /// Role declarations alone are not verification progress. A pending member
    /// may have no known role yet; requested Git mappings stay in its request.
    /// Zero also represents an orphan with no retained role declaration after
    /// selection expiry. It does not assert availability or verification.
    pub roles: u8,
    pub git: Vec<GitIdentity>,
    pub location: Location,
    /// Caller-defined metadata, included in the immutable block identity.
    /// Its schema and meaning must be validated by the caller. The format only
    /// preserves these bytes and bounds their size; it never authorizes them.
    pub attributes: Vec<u8>,
}
impl Entry {
    fn validate(&self) -> Result<()> {
        if self.attributes.len() > MAX_BYTES {
            return Err(Error::Limit("member attributes"));
        }
        if self.git.len() > 8 {
            return Err(Error::Format("Git identity count"));
        }
        if self.location == Location::Pending && !self.git.is_empty() {
            return Err(Error::Format("pending Git mapping"));
        }
        let mut roles = 0;
        let mut kinds = BTreeSet::new();
        let mut previous = None;
        for git in &self.git {
            if git.oid.bytes().iter().all(|v| *v == 0)
                || previous.is_some_and(|p: ObjectId| p >= git.oid)
                || !kinds.insert((git.oid.algorithm(), git.kind))
            {
                return Err(Error::Format("Git identities"));
            }
            previous = Some(git.oid);
            roles |= git.kind.role();
        }
        if self.roles & 0x0f != roles {
            return Err(Error::Format("Git roles"));
        }
        if let Location::Segment { offset, .. } = self.location
            && (offset < 64 || offset.checked_add(self.bytes).is_none())
        {
            return Err(Error::Format("segment extent"));
        }
        Ok(())
    }
    fn normalize(&mut self) -> Result<()> {
        self.git.sort();
        self.git.dedup();
        self.validate()
    }
    fn merge(&mut self, other: Self) -> Result<()> {
        if self.bytes != other.bytes
            || self.location != other.location
            || self.attributes != other.attributes
        {
            return Err(Error::Conflict("canonical size, location or attributes"));
        }
        self.roles |= other.roles;
        self.git.extend(other.git);
        self.normalize()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    id: Digest,
    entries: Vec<Entry>,
    encoded: Vec<u8>,
}
impl Block {
    /// Canonicalize a single bounded block. Exact duplicate members collapse;
    /// conflicting sizes, locations or Git mappings fail without output.
    pub fn build(entries: Vec<Entry>, limits: Limits) -> Result<Self> {
        limits.validate()?;
        if entries.len() > limits.members {
            return Err(Error::Limit("input members"));
        }
        Self::from_sorted(canonicalize(entries)?, limits)
    }
    fn from_sorted(entries: Vec<Entry>, limits: Limits) -> Result<Self> {
        if entries.len() > limits.members {
            return Err(Error::Limit("members"));
        }
        let encoded = codec::encode(&entries, limits.bytes)?;
        let id = encoded[encoded.len() - 32..].try_into().unwrap();
        Ok(Self {
            id,
            entries,
            encoded,
        })
    }
    /// The expected ID must come from the caller's trusted root or pin.
    /// Accepting a self-reported checksum cannot establish directory identity.
    pub fn decode(bytes: &[u8], expected: &Digest, limits: Limits) -> Result<Self> {
        limits.validate()?;
        let entries = codec::decode(bytes, expected, limits)?;
        validate_global(&entries)?;
        Ok(Self {
            id: *expected,
            entries,
            encoded: bytes.to_vec(),
        })
    }
    pub fn id(&self) -> &Digest {
        &self.id
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }
    pub fn find(&self, digest: &Digest) -> Option<(usize, &Entry)> {
        self.entries
            .binary_search_by_key(digest, |e| e.digest)
            .ok()
            .map(|index| (index, &self.entries[index]))
    }
}

/// Deterministic bulk construction for migration and equivalence experiments.
/// Input memory belongs to the caller; individual output blocks are bounded.
/// This is not an incremental production directory or authorization cache.
pub fn paginate(entries: Vec<Entry>, limits: Limits) -> Result<Vec<Block>> {
    limits.validate()?;
    let entries = canonicalize(entries)?;
    let mut blocks = Vec::new();
    let mut page = Vec::new();
    let mut size = codec::OVERHEAD;
    for entry in entries {
        let bytes = codec::entry_size(&entry);
        if bytes > limits.bytes - codec::OVERHEAD {
            return Err(Error::Limit("entry bytes"));
        }
        if page.len() == limits.members || size + bytes > limits.bytes {
            blocks.push(Block::from_sorted(std::mem::take(&mut page), limits)?);
            size = codec::OVERHEAD;
        }
        size += bytes;
        page.push(entry);
    }
    if !page.is_empty() {
        blocks.push(Block::from_sorted(page, limits)?);
    }
    Ok(blocks)
}

fn canonicalize(entries: Vec<Entry>) -> Result<Vec<Entry>> {
    let mut unique = BTreeMap::<Digest, Entry>::new();
    for mut entry in entries {
        entry.normalize()?;
        match unique.entry(entry.digest) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(entry);
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                slot.get_mut().merge(entry)?;
            }
        }
    }
    let entries: Vec<_> = unique.into_values().collect();
    validate_global(&entries)?;
    Ok(entries)
}

fn validate_global(entries: &[Entry]) -> Result<()> {
    let mut oids = BTreeSet::new();
    let mut previous = None;
    for entry in entries {
        entry.validate()?;
        if previous.is_some_and(|p| p >= entry.digest) {
            return Err(Error::Format("member order or duplicate"));
        }
        previous = Some(entry.digest);
        for git in &entry.git {
            if !oids.insert(git.oid) {
                return Err(Error::Conflict("Git mapping"));
            }
        }
    }
    Ok(())
}
