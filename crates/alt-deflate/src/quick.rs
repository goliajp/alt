//! The general compressor: valid DEFLATE chosen for speed, not for agreeing with
//! zlib byte for byte. Matches are found through chains of positions whose next four
//! bytes hash alike, searched directly in the input.

use crate::{bits::BitWriter, block::Block};
use std::cell::RefCell;

const WINDOW: usize = 32768;
const MIN_MATCH: usize = 4;
const MAX_MATCH: usize = 258;
/// stored blocks carry their length in sixteen bits
const MAX_STORED: usize = 65535;

/// how hard one level searches
struct Effort {
    /// candidates examined per position
    chain: u32,
    /// a match this long ends the search
    nice: usize,
    /// a match shorter than this is held back to see if the next byte starts a longer one
    lazy: usize,
    /// when the match held back is at least this long, the next byte gets a quarter of the search
    good: usize,
    /// matches up to this length have their inside indexed
    index: usize,
}

const fn tuning(chain: u32, nice: usize, lazy: usize, good: usize, index: usize) -> Effort {
    Effort {
        chain,
        nice,
        lazy,
        good,
        index,
    }
}

const EFFORTS: [Effort; 9] = [
    tuning(1, 16, 0, 0, 0),
    tuning(2, 32, 0, 0, 8),
    tuning(4, 64, 0, 0, 16),
    tuning(8, 96, 16, 8, 258),
    tuning(16, 128, 16, 8, 258),
    tuning(32, 160, 16, 8, 258),
    tuning(64, 258, 32, 8, 258),
    tuning(256, 258, 128, 32, 258),
    tuning(1024, 258, 258, 32, 258),
];

/// tables that are too large to set up for every small input
struct Scratch {
    /// position plus one of the latest string with each hash; zero when none
    head: Vec<u32>,
    /// position plus one of the previous string with the same hash, by position modulo the window
    previous: Vec<u32>,
    block: Block,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch {
        head: Vec::new(),
        previous: vec![0; WINDOW],
        block: Block::quick(),
    });
}

struct Found {
    length: usize,
    distance: usize,
}

struct Finder<'a> {
    data: &'a [u8],
    head: &'a mut [u32],
    previous: &'a mut [u32],
    shift: u32,
    /// positions below this are on their chains
    inserted: usize,
    /// the last position that still has four bytes to hash
    last: usize,
}

impl Finder<'_> {
    /// `at` must not be past `last`
    #[inline(always)]
    fn hash(&self, at: usize) -> usize {
        debug_assert!(at <= self.last && self.data.len() >= MIN_MATCH);
        // four bytes are readable at every position up to `last`
        let word = unsafe { self.data.as_ptr().add(at).cast::<u32>().read_unaligned() };
        (word.to_le().wrapping_mul(0x9e37_79b1) >> self.shift) as usize
    }

    /// puts `at` on its chain and returns what was the head of that chain
    #[inline(always)]
    fn link(&mut self, at: usize) -> u32 {
        let slot = self.hash(at);
        // the hash is shifted down to the size of `head`, and `previous` holds one
        // entry per position modulo the window
        unsafe {
            let head = self.head.get_unchecked_mut(slot);
            let before = *head;
            *self.previous.get_unchecked_mut(at % WINDOW) = before;
            *head = at as u32 + 1;
            before
        }
    }

    /// puts every position before `end` on its chain
    #[inline(always)]
    fn insert_until(&mut self, end: usize) {
        let end = end.min(self.last + 1);
        while self.inserted < end {
            self.link(self.inserted);
            self.inserted += 1;
        }
    }

    /// The longest match at `at` that is longer than `floor`, among the first
    /// candidates of its chain. `at` must not be past `last`.
    #[inline(always)]
    fn find(&mut self, at: usize, floor: usize, chain: u32, effort: &Effort) -> Found {
        self.insert_until(at);
        let mut candidate = self.link(at);
        self.inserted = at + 1;

        let data = self.data;
        let longest = MAX_MATCH.min(data.len() - at);
        let nice = effort.nice.min(longest);
        let mut best = Found {
            length: floor,
            distance: 0,
        };
        let mut chain = chain;
        // Candidates are earlier positions of this input, so `from < at`; `at` has at
        // least four bytes after it, and comparisons stop at `longest`, which ends
        // inside the input.
        unsafe {
            let base = data.as_ptr();
            let here = base.add(at);
            let first = here.cast::<u32>().read_unaligned();
            while candidate != 0 && chain != 0 {
                let from = candidate as usize - 1;
                if at - from > WINDOW {
                    break;
                }
                let there = base.add(from);
                // a longer match agrees on the first four bytes and on the byte where
                // the best one so far stops
                if best.length < longest
                    && *there.add(best.length) == *here.add(best.length)
                    && there.cast::<u32>().read_unaligned() == first
                {
                    let length = common(there, here, longest);
                    if length > best.length {
                        best = Found {
                            length,
                            distance: at - from,
                        };
                        if length >= nice {
                            break;
                        }
                    }
                }
                let next = *self.previous.get_unchecked(from % WINDOW);
                // an overwritten link no longer leads backwards
                if next >= candidate {
                    break;
                }
                candidate = next;
                chain -= 1;
            }
        }
        best
    }
}

/// How many leading bytes at `a` and `b` are equal, up to `longest`, which must be
/// readable at both.
#[inline(always)]
unsafe fn common(a: *const u8, b: *const u8, longest: usize) -> usize {
    unsafe {
        let mut equal = 0;
        while equal + 8 <= longest {
            let differ = a.add(equal).cast::<u64>().read_unaligned()
                ^ b.add(equal).cast::<u64>().read_unaligned();
            if differ != 0 {
                return equal + (differ.to_le().trailing_zeros() / 8) as usize;
            }
            equal += 8;
        }
        while equal < longest && *a.add(equal) == *b.add(equal) {
            equal += 1;
        }
        equal
    }
}

/// The fastest level: one candidate per position, the latest with the same hash, and
/// no chains at all. Returns where the search stopped and where the open block
/// starts; the caller finishes it. `data` holds at least four bytes.
fn single_probe(
    data: &[u8],
    head: &mut [u32],
    shift: u32,
    block: &mut Block,
    out: &mut BitWriter,
) -> (usize, usize) {
    let last = data.len() - MIN_MATCH;
    let mut block_start = 0;
    let mut at = 0;
    while at <= last {
        // four bytes are readable up to `last`; the hash is shifted to the table's size;
        // a candidate is an earlier position, and comparisons end inside the input
        let (candidate, word) = unsafe {
            let word = data.as_ptr().add(at).cast::<u32>().read_unaligned();
            let slot = (word.to_le().wrapping_mul(0x9e37_79b1) >> shift) as usize;
            let head = head.get_unchecked_mut(slot);
            let candidate = *head;
            *head = at as u32 + 1;
            (candidate as usize, word)
        };
        let full = if candidate != 0
            && at - (candidate - 1) <= WINDOW
            && unsafe {
                data.as_ptr()
                    .add(candidate - 1)
                    .cast::<u32>()
                    .read_unaligned()
            } == word
        {
            let from = candidate - 1;
            let longest = MAX_MATCH.min(data.len() - at);
            let length = unsafe { common(data.as_ptr().add(from), data.as_ptr().add(at), longest) };
            at += length;
            block.tally_match(at - length - from, length)
        } else {
            at += 1;
            block.tally_literal(data[at - 1])
        };
        if full {
            let raw = &data[block_start..at];
            block.flush(out, (raw.len() <= MAX_STORED).then_some(raw), false);
            block_start = at;
        }
    }
    (at, block_start)
}

/// compresses `data` at `level` (1..=9) behind what `out` already holds
pub(crate) fn deflate(data: &[u8], level: u8, out: Vec<u8>) -> Vec<u8> {
    let effort = &EFFORTS[level as usize - 1];
    SCRATCH.with_borrow_mut(|scratch| {
        // a table about as large as the input: clearing more would cost more than it finds
        let bits = (data.len().max(1).ilog2() + 1).clamp(8, 16);
        scratch.head.clear();
        scratch.head.resize(1 << bits, 0);
        let mut finder = Finder {
            data,
            head: &mut scratch.head,
            previous: &mut scratch.previous,
            shift: 32 - bits,
            inserted: 0,
            last: data.len().saturating_sub(MIN_MATCH),
        };
        let block = &mut scratch.block;
        let mut out = BitWriter::new(out);
        let mut block_start = 0;
        let mut at = 0;
        if effort.chain == 1 && data.len() >= MIN_MATCH {
            (at, block_start) = single_probe(data, finder.head, finder.shift, block, &mut out);
        }
        let mut misses = 0u32;
        let searchable = if data.len() >= MIN_MATCH {
            finder.last + 1
        } else {
            0
        };
        while at < data.len() {
            let mut full = false;
            let mut found = Found {
                length: 0,
                distance: 0,
            };
            if at < searchable {
                found = finder.find(at, MIN_MATCH - 1, effort.chain, effort);
            }
            if found.distance == 0 {
                full = block.tally_literal(data[at]);
                at += 1;
                misses += 1;
                // data that has not matched for a while is passed through in steps
                // that grow with the stretch; only the fast levels do this
                if effort.lazy == 0 && misses >= 64 {
                    let skip = ((misses >> 6) as usize).min(data.len() - at).min(32);
                    for byte in &data[at..at + skip] {
                        full |= block.tally_literal(*byte);
                    }
                    at += skip;
                    finder.inserted = finder.inserted.max(at.min(finder.last + 1));
                }
            } else {
                misses = 0;
                // hold a short match back while the next byte starts a longer one
                while found.length < effort.lazy && at + 1 < searchable {
                    let chain = if found.length >= effort.good {
                        effort.chain >> 2
                    } else {
                        effort.chain
                    };
                    let next = finder.find(at + 1, found.length, chain.max(1), effort);
                    if next.distance == 0 {
                        break;
                    }
                    full |= block.tally_literal(data[at]);
                    at += 1;
                    found = next;
                }
                full |= block.tally_match(found.distance, found.length);
                at += found.length;
                if found.length <= effort.index {
                    finder.insert_until(at);
                } else {
                    // the fast levels do not index the inside of longer matches
                    finder.inserted = finder.inserted.max(at.min(finder.last + 1));
                }
            }
            if full {
                let raw = &data[block_start..at];
                block.flush(&mut out, (raw.len() <= MAX_STORED).then_some(raw), false);
                block_start = at;
            }
        }
        let raw = &data[block_start..];
        block.flush(&mut out, (raw.len() <= MAX_STORED).then_some(raw), true);
        out.finish()
    })
}
