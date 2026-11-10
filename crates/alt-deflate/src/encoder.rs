//! The per-level parsing strategies.

use crate::bits::BitWriter;
use crate::block::Block;
use crate::tables::MIN_MATCH;
use crate::window::{MAX_DIST, MIN_LOOKAHEAD, Search, Window};

/// a minimum-length match further back than this costs more than literals
const TOO_FAR: usize = 4096;

struct Level {
    search: Search,
    /// greedy levels: longest match whose inner positions are still hashed;
    /// lazy levels: a match this long is taken without trying the next byte
    max_lazy: usize,
    lazy: bool,
}

const fn level(good_length: usize, max_lazy: usize, nice_length: usize, max_chain: usize) -> Level {
    Level {
        search: Search {
            good_length,
            nice_length,
            max_chain,
        },
        max_lazy,
        lazy: false,
    }
}

const fn lazy(good_length: usize, max_lazy: usize, nice_length: usize, max_chain: usize) -> Level {
    Level {
        lazy: true,
        ..level(good_length, max_lazy, nice_length, max_chain)
    }
}

/// levels 1 to 9
const LEVELS: [Level; 9] = [
    level(4, 4, 8, 4),
    level(4, 5, 16, 8),
    level(4, 6, 32, 32),
    lazy(4, 4, 16, 16),
    lazy(8, 16, 32, 32),
    lazy(8, 16, 128, 128),
    lazy(8, 32, 128, 256),
    lazy(32, 128, 258, 1024),
    lazy(32, 258, 258, 4096),
];

struct Encoder<'a> {
    window: Window<'a>,
    block: Block,
    out: BitWriter,
}

/// Appends the deflate stream of `data` at `level` (1..=9) to `out`.
pub fn deflate(data: &[u8], level: u8, out: Vec<u8>) -> Vec<u8> {
    let config = &LEVELS[level as usize - 1];
    let mut encoder = Encoder {
        window: Window::new(data),
        block: Block::new(),
        out: BitWriter::new(out),
    };
    if config.lazy {
        encoder.run_lazy(config);
    } else {
        encoder.run_greedy(config);
    }
    encoder.flush_block(true);
    encoder.out.finish()
}

impl Encoder<'_> {
    fn flush_block(&mut self, last: bool) {
        let raw = self.window.block_bytes();
        self.block.flush(&mut self.out, raw, last);
        self.window.block_start = self.window.strstart as isize;
    }

    /// Returns false at the end of the input.
    fn ensure_lookahead(&mut self) -> bool {
        if self.window.lookahead < MIN_LOOKAHEAD {
            self.window.fill();
        }
        self.window.lookahead != 0
    }

    /// Inserts the current position and returns a chain head within reach.
    fn insert_current(&mut self) -> Option<usize> {
        let w = &mut self.window;
        if w.lookahead < MIN_MATCH {
            return None;
        }
        let head = w.insert(w.strstart);
        (head != 0 && w.strstart - head <= MAX_DIST).then_some(head)
    }

    /// Takes every match as soon as it is found.
    fn run_greedy(&mut self, config: &Level) {
        // zlib keeps the last search result when nothing is searched
        let mut match_length = MIN_MATCH - 1;
        while self.ensure_lookahead() {
            if let Some(head) = self.insert_current() {
                match_length = self
                    .window
                    .longest_match(head, MIN_MATCH - 1, &config.search);
            }
            let w = &mut self.window;
            let full;
            if match_length >= MIN_MATCH {
                full = self
                    .block
                    .tally_match(w.strstart - w.match_start, match_length);
                w.lookahead -= match_length;
                if match_length <= config.max_lazy && w.lookahead >= MIN_MATCH {
                    for _ in 1..match_length {
                        w.strstart += 1;
                        w.insert(w.strstart);
                    }
                    w.strstart += 1;
                } else {
                    w.strstart += match_length;
                    w.restart_hash();
                }
                match_length = 0;
            } else {
                full = self.block.tally_literal(w.bytes[w.strstart]);
                w.lookahead -= 1;
                w.strstart += 1;
            }
            if full {
                self.flush_block(false);
            }
        }
    }

    /// Defers each match by one byte and drops it if the next position
    /// has a longer one.
    fn run_lazy(&mut self, config: &Level) {
        let mut match_length = MIN_MATCH - 1;
        let mut literal_pending = false;
        while self.ensure_lookahead() {
            let head = self.insert_current();
            let w = &mut self.window;
            let prev_length = match_length;
            let prev_match = w.match_start;
            match_length = MIN_MATCH - 1;

            if let Some(head) = head
                && prev_length < config.max_lazy
            {
                match_length = w.longest_match(head, prev_length, &config.search);
                // match_start is stale when nothing longer was found; the
                // result is then discarded either way
                if match_length == MIN_MATCH && w.strstart.wrapping_sub(w.match_start) > TOO_FAR {
                    match_length = MIN_MATCH - 1;
                }
            }

            if prev_length >= MIN_MATCH && match_length <= prev_length {
                // the last two positions of the input are never hashed
                let max_insert = w.strstart + w.lookahead - MIN_MATCH;
                let full = self
                    .block
                    .tally_match(w.strstart - 1 - prev_match, prev_length);
                w.lookahead -= prev_length - 1;
                for _ in 0..prev_length - 2 {
                    w.strstart += 1;
                    if w.strstart <= max_insert {
                        w.insert(w.strstart);
                    }
                }
                literal_pending = false;
                match_length = MIN_MATCH - 1;
                w.strstart += 1;
                if full {
                    self.flush_block(false);
                }
            } else if literal_pending {
                if self.block.tally_literal(w.bytes[w.strstart - 1]) {
                    self.flush_block(false);
                }
                self.window.strstart += 1;
                self.window.lookahead -= 1;
            } else {
                literal_pending = true;
                w.strstart += 1;
                w.lookahead -= 1;
            }
        }
        if literal_pending {
            let w = &self.window;
            self.block.tally_literal(w.bytes[w.strstart - 1]);
        }
    }
}
