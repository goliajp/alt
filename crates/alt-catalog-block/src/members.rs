use crate::{Block, Digest, Error, Result, codec::Cursor};

const MAGIC: &[u8; 8] = b"ALTSET01";

/// Exact membership in one immutable block. It carries no owner, expiry,
/// repository, verification progress or authority; those remain in the DB.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberSet {
    block: Digest,
    count: usize,
    bits: Vec<u8>,
}
impl MemberSet {
    pub fn empty(block: &Block) -> Self {
        let count = block.entries().len();
        Self {
            block: *block.id(),
            count,
            bits: vec![0; count.div_ceil(8)],
        }
    }
    pub fn from_indices(block: &Block, indices: impl IntoIterator<Item = usize>) -> Result<Self> {
        let mut set = Self::empty(block);
        for index in indices {
            if index >= set.count {
                return Err(Error::Format("member index"));
            }
            set.bits[index / 8] |= 1 << (index % 8);
        }
        Ok(set)
    }
    pub fn block_id(&self) -> &Digest {
        &self.block
    }
    pub fn member_count(&self) -> usize {
        self.count
    }
    pub fn contains(&self, index: usize) -> bool {
        index < self.count && self.bits[index / 8] & (1 << (index % 8)) != 0
    }
    pub fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.count).filter(|i| self.contains(*i))
    }
    pub fn union(&self, other: &Self) -> Result<Self> {
        self.combine(other, |a, b| a | b)
    }
    pub fn intersection(&self, other: &Self) -> Result<Self> {
        self.combine(other, |a, b| a & b)
    }
    pub fn difference(&self, other: &Self) -> Result<Self> {
        self.combine(other, |a, b| a & !b)
    }
    fn combine(&self, other: &Self, op: impl Fn(u8, u8) -> u8) -> Result<Self> {
        if self.block != other.block || self.count != other.count {
            return Err(Error::Conflict("member set block"));
        }
        Ok(Self {
            block: self.block,
            count: self.count,
            bits: self
                .bits
                .iter()
                .zip(&other.bits)
                .map(|(&a, &b)| op(a, b))
                .collect(),
        })
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(76 + self.bits.len());
        out.extend(MAGIC);
        out.extend(self.block);
        out.extend((self.count as u32).to_le_bytes());
        out.extend(&self.bits);
        let hash = blake3::hash(&out);
        out.extend(hash.as_bytes());
        out
    }
    /// Check block binding, exact byte length, unused bits and checksum before
    /// returning a set. A checksum detects damage, not a forged owner or pin.
    pub fn decode(bytes: &[u8], block: &Block) -> Result<Self> {
        let count = block.entries().len();
        if bytes.len() != 76 + count.div_ceil(8) {
            return Err(Error::Format("member set length"));
        }
        let end = bytes.len() - 32;
        if blake3::hash(&bytes[..end]).as_bytes() != &bytes[end..] {
            return Err(Error::Format("member set checksum"));
        }
        let mut input = Cursor(&bytes[..end]);
        if &input.take::<8>()? != MAGIC
            || &input.take::<32>()? != block.id()
            || u32::from_le_bytes(input.take()?) as usize != count
        {
            return Err(Error::Format("member set binding"));
        }
        if !count.is_multiple_of(8)
            && input
                .0
                .last()
                .is_some_and(|b| b & (0xff << (count % 8)) != 0)
        {
            return Err(Error::Format("unused member bits"));
        }
        Ok(Self {
            block: *block.id(),
            count,
            bits: input.0.to_vec(),
        })
    }
}
