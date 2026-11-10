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
}

const EFFORTS: [Effort; 9] = [
    Effort {
        chain: 2,
        nice: 32,
        lazy: 0,
    },
    Effort {
        chain: 4,
        nice: 64,
        lazy: 0,
    },
    Effort {
        chain: 8,
        nice: 96,
        lazy: 0,
    },
    Effort {
        chain: 8,
        nice: 96,
        lazy: 16,
    },
    Effort {
        chain: 16,
        nice: 128,
        lazy: 32,
    },
    Effort {
        chain: 32,
        nice: 160,
        lazy: 64,
    },
    Effort {
        chain: 64,
        nice: 258,
        lazy: 128,
    },
    Effort {
        chain: 256,
        nice: 258,
        lazy: 258,
    },
    Effort {
        chain: 1024,
        nice: 258,
        lazy: 258,
    },
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
        block: Block::new(),
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
    #[inline(always)]
    fn hash(&self, at: usize) -> usize {
        let word = u32::from_le_bytes(*self.data[at..].first_chunk::<4>().unwrap());
        (word.wrapping_mul(0x9e37_79b1) >> self.shift) as usize
    }

    /// puts every position before `end` on its chain
    #[inline(always)]
    fn insert_until(&mut self, end: usize) {
        let end = end.min(self.last + 1);
        while self.inserted < end {
            let slot = self.hash(self.inserted);
            self.previous[self.inserted % WINDOW] = self.head[slot];
            self.head[slot] = self.inserted as u32 + 1;
            self.inserted += 1;
        }
    }

    /// The longest match at `at` that is longer than `floor`, among the first
    /// candidates of its chain. `at` must not be past `last`.
    #[inline(always)]
    fn find(&mut self, at: usize, floor: usize, effort: &Effort) -> Found {
        self.insert_until(at);
        let slot = self.hash(at);
        let mut candidate = self.head[slot];
        self.previous[at % WINDOW] = candidate;
        self.head[slot] = at as u32 + 1;
        self.inserted = at + 1;

        let data = self.data;
        let longest = MAX_MATCH.min(data.len() - at);
        let nice = effort.nice.min(longest);
        let mut best = Found {
            length: floor,
            distance: 0,
        };
        let mut chain = effort.chain;
        while candidate != 0 && chain != 0 {
            let from = candidate as usize - 1;
            if at - from > WINDOW {
                break;
            }
            // a longer match must agree at the byte the best one stops at
            if best.length < longest && data[from + best.length] == data[at + best.length] {
                let length = common(&data[from..], &data[at..at + longest]);
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
            let next = self.previous[from % WINDOW];
            // an overwritten link no longer leads backwards
            if next >= candidate {
                break;
            }
            candidate = next;
            chain -= 1;
        }
        best
    }
}

/// how many leading bytes of `a` and `b` are equal; `b` is the shorter
#[inline(always)]
fn common(a: &[u8], b: &[u8]) -> usize {
    let mut equal = 0;
    while let (Some(x), Some(y)) = (a[equal..].first_chunk::<8>(), b[equal..].first_chunk::<8>()) {
        let differ = u64::from_le_bytes(*x) ^ u64::from_le_bytes(*y);
        if differ != 0 {
            return equal + (differ.trailing_zeros() / 8) as usize;
        }
        equal += 8;
    }
    while equal < b.len() && a[equal] == b[equal] {
        equal += 1;
    }
    equal
}

/// compresses `data` at `level` (1..=9) behind what `out` already holds
pub(crate) fn deflate(data: &[u8], level: u8, out: Vec<u8>) -> Vec<u8> {
    let effort = &EFFORTS[level as usize - 1];
    SCRATCH.with_borrow_mut(|scratch| {
        // a table about as large as the input: clearing more would cost more than it finds
        let bits = (data.len().max(1).ilog2() + 1).clamp(8, 15);
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
                found = finder.find(at, MIN_MATCH - 1, effort);
            }
            if found.distance == 0 {
                full = block.tally_literal(data[at]);
                at += 1;
            } else {
                // hold a short match back while the next byte starts a longer one
                while found.length < effort.lazy && at + 1 < searchable {
                    let next = finder.find(at + 1, found.length, effort);
                    if next.distance == 0 {
                        break;
                    }
                    full |= block.tally_literal(data[at]);
                    at += 1;
                    found = next;
                }
                full |= block.tally_match(found.distance, found.length);
                at += found.length;
                if effort.lazy != 0 || found.length <= 16 {
                    finder.insert_until(at);
                } else {
                    // the fastest levels do not index the inside of long matches
                    finder.inserted = at;
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
