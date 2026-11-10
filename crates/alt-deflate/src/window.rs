//! The sliding window, its hash chains and the match search.

use crate::tables::{MAX_MATCH, MIN_MATCH};

/// lookahead kept ahead of the current position except at the end of input
pub const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const W_SIZE: usize = 1 << 15;
const W_MASK: usize = W_SIZE - 1;
/// matches never reach further back than this
pub const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;

const HASH_BITS: usize = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: usize = HASH_SIZE - 1;
const HASH_SHIFT: usize = HASH_BITS.div_ceil(MIN_MATCH);

/// Limits of the match search for one compression level.
pub struct Search {
    /// search a quarter of the chain once a match this long is known
    pub good_length: usize,
    /// stop searching at a match this long
    pub nice_length: usize,
    pub max_chain: usize,
}

pub struct Window<'a> {
    /// input not yet copied into the window
    input: &'a [u8],
    pub bytes: Vec<u8>,
    /// most recent position for each hash; 0 is the chain end, so position
    /// 0 can never be matched
    head: Vec<u16>,
    /// previous position with the same hash, indexed by position mod window
    prev: Vec<u16>,
    hash: usize,
    pub strstart: usize,
    pub lookahead: usize,
    /// start of the current block; negative once it slid out of the window
    pub block_start: isize,
    /// start of the last match found; stale when no match was found
    pub match_start: usize,
}

impl<'a> Window<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            bytes: vec![0; 2 * W_SIZE],
            head: vec![0; HASH_SIZE],
            prev: vec![0; W_SIZE],
            hash: 0,
            strstart: 0,
            lookahead: 0,
            block_start: 0,
            match_start: 0,
        }
    }

    /// The input of the current block, if all of it is still in the window.
    pub fn block_bytes(&self) -> Option<&[u8]> {
        let start = usize::try_from(self.block_start).ok()?;
        Some(&self.bytes[start..self.strstart])
    }

    /// Refills the lookahead. The window slides only here, and only when
    /// the position is within MIN_LOOKAHEAD of its end; both the timing and
    /// the fill-to-the-end read size decide which matches zlib can see.
    pub fn fill(&mut self) {
        loop {
            let mut room = 2 * W_SIZE - self.lookahead - self.strstart;
            if self.strstart >= W_SIZE + MAX_DIST {
                self.bytes.copy_within(W_SIZE..2 * W_SIZE - room, 0);
                self.match_start = self.match_start.wrapping_sub(W_SIZE);
                self.strstart -= W_SIZE;
                self.block_start -= W_SIZE as isize;
                self.slide_hash();
                room += W_SIZE;
            }
            if self.input.is_empty() {
                break;
            }
            let n = room.min(self.input.len());
            let end = self.strstart + self.lookahead;
            self.bytes[end..end + n].copy_from_slice(&self.input[..n]);
            self.input = &self.input[n..];
            self.lookahead += n;
            if self.lookahead >= MIN_MATCH {
                self.restart_hash();
            }
            if self.lookahead >= MIN_LOOKAHEAD || self.input.is_empty() {
                break;
            }
        }
    }

    fn slide_hash(&mut self) {
        for slot in self.head.iter_mut().chain(self.prev.iter_mut()) {
            *slot = slot.saturating_sub(W_SIZE as u16);
        }
    }

    fn update_hash(&mut self, byte: u8) {
        self.hash = ((self.hash << HASH_SHIFT) ^ byte as usize) & HASH_MASK;
    }

    /// Seeds the rolling hash with the two bytes at the current position.
    pub fn restart_hash(&mut self) {
        self.hash = self.bytes[self.strstart] as usize;
        self.update_hash(self.bytes[self.strstart + 1]);
    }

    /// Puts the string at `pos` on its hash chain and returns the previous
    /// head of that chain. Positions must be inserted in order.
    pub fn insert(&mut self, pos: usize) -> usize {
        self.update_hash(self.bytes[pos + MIN_MATCH - 1]);
        let head = self.head[self.hash];
        self.prev[pos & W_MASK] = head;
        self.head[self.hash] = pos as u16;
        head as usize
    }

    /// Walks the hash chain from `candidate` and returns the length of the
    /// longest match longer than `prev_length`, setting `match_start`;
    /// returns at most `prev_length` (clamped to the lookahead) when there
    /// is none. Among equal lengths the nearest match wins.
    pub fn longest_match(
        &mut self,
        mut candidate: usize,
        prev_length: usize,
        search: &Search,
    ) -> usize {
        let scan = self.strstart;
        let bytes = &self.bytes[..];
        let mut chain = search.max_chain;
        if prev_length >= search.good_length {
            chain >>= 2;
        }
        let nice = search.nice_length.min(self.lookahead);
        let limit = scan.saturating_sub(MAX_DIST);
        let mut best = prev_length;
        let mut end = [bytes[scan + best - 1], bytes[scan + best]];

        loop {
            if bytes[candidate + best] == end[1]
                && bytes[candidate + best - 1] == end[0]
                && bytes[candidate] == bytes[scan]
                && bytes[candidate + 1] == bytes[scan + 1]
            {
                // the third byte is equal because the hashes are
                let len = MIN_MATCH + common_prefix(bytes, candidate + MIN_MATCH, scan + MIN_MATCH);
                if len > best {
                    self.match_start = candidate;
                    best = len;
                    if len >= nice {
                        break;
                    }
                    end = [bytes[scan + best - 1], bytes[scan + best]];
                }
            }
            candidate = self.prev[candidate & W_MASK] as usize;
            chain -= 1;
            if candidate <= limit || chain == 0 {
                break;
            }
        }
        best.min(self.lookahead)
    }
}

/// Number of equal bytes at `a` and `b`, at most MAX_MATCH - MIN_MATCH.
fn common_prefix(bytes: &[u8], a: usize, b: usize) -> usize {
    const SPAN: usize = MAX_MATCH - MIN_MATCH;
    let x = &bytes[a..a + SPAN];
    let y = &bytes[b..b + SPAN];
    let (x_words, x_tail) = x.as_chunks::<8>();
    let (y_words, y_tail) = y.as_chunks::<8>();
    for (n, (wx, wy)) in x_words.iter().zip(y_words).enumerate() {
        let diff = u64::from_le_bytes(*wx) ^ u64::from_le_bytes(*wy);
        if diff != 0 {
            return n * 8 + (diff.trailing_zeros() / 8) as usize;
        }
    }
    let equal_tail = x_tail.iter().zip(y_tail).take_while(|(p, q)| p == q);
    x_words.len() * 8 + equal_tail.count()
}
