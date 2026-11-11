//! Frame encoder: block loop, block framing and the working memory kept
//! between calls.

mod dfast;
mod fast;
mod fse;
mod huf;
mod lit;
mod matcher;
mod pairs;
mod params;
mod search;
mod seq;
mod split;

use std::cell::RefCell;

use crate::frame::{BLOCK_MAX, MAGIC};
use lit::{LiteralCoder, min_gain};
use matcher::{SeqStore, Window};
use params::{Params, WINDOW_LOG_MAX};
use search::Matcher;
use seq::SequenceCoder;

/// Data handled per set of hash tables; keeps every index within 32 bits.
const EPOCH: usize = 1 << 30;

/// Most table entries (both tables together) kept between calls.
const KEPT_ENTRIES: usize = 3 << 16;

/// Hash tables kept per thread, so that compressing many small inputs does
/// not allocate and clear tables for each. They are never cleared: every
/// frame takes indices above all that the tables hold, which makes entries
/// left by earlier frames read as too old to be candidates.
struct Tables {
    long: Vec<u32>,
    short: Vec<u32>,
    /// lowest index not yet handed out
    next: usize,
}

impl Tables {
    /// Makes the tables at least as long as asked and returns the first of
    /// `span` indices that no entry in them reaches.
    fn claim(&mut self, long: usize, short: usize, span: usize) -> usize {
        if self.long.len() < long {
            self.long.resize(long, 0);
        }
        if self.short.len() < short {
            self.short.resize(short, 0);
        }
        if self.next + span >= u32::MAX as usize {
            self.long.fill(0);
            self.short.fill(0);
            self.next = 1;
        }
        let first = self.next;
        self.next += span;
        first
    }
}

/// What turns the blocks of a frame into bytes.
struct BlockCoder {
    store: SeqStore,
    literals: LiteralCoder,
    sequences: SequenceCoder,
}

impl BlockCoder {
    /// Appends the block `block`, which starts at index `start` of `window`.
    /// `first` tells whether it opens the frame.
    fn block(
        &mut self,
        matcher: &mut Matcher<'_>,
        window: &Window<'_>,
        start: usize,
        block: &[u8],
        (first, last): (bool, bool),
        out: &mut Vec<u8>,
    ) {
        let reps = matcher.reps;
        matcher.block(window, start, start + block.len(), &mut self.store);
        let saved = (self.literals.save(), self.sequences.save());
        let header = out.len();
        out.extend_from_slice(&[0; 3]);
        let count = self.store.sequences.len();
        self.literals.encode(&self.store.literals, count, out);
        self.sequences.encode(&self.store.sequences, out);
        let packed = out.len() - header - 3;
        if packed + min_gain(block.len()) < block.len() {
            block_header(&mut out[header..header + 3], last, 2, packed);
            return;
        }
        // not worth it: the decoder's offsets and tables stay as they were
        // before this block
        out.truncate(header + 3);
        matcher.reps = reps;
        self.literals.restore(saved.0);
        self.sequences.restore(saved.1);
        // some older decoders reject a frame whose first block is a
        // single-byte block
        if !first && block.iter().all(|&byte| byte == block[0]) {
            block_header(&mut out[header..header + 3], last, 1, block.len());
            out.push(block[0]);
        } else {
            block_header(&mut out[header..header + 3], last, 0, block.len());
            out.extend_from_slice(block);
        }
    }
}

struct Scratch {
    tables: Tables,
    coder: BlockCoder,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch {
        tables: Tables {
            long: Vec::new(),
            short: Vec::new(),
            next: 1,
        },
        coder: BlockCoder {
            store: SeqStore::new(),
            literals: LiteralCoder::new(),
            sequences: SequenceCoder::new(),
        },
    });
}

fn write_header(out: &mut Vec<u8>, len: usize, single_segment: bool, window_log: u32) {
    out.extend_from_slice(&MAGIC.to_le_bytes());
    let size = len as u64;
    let (flag, bytes) = match size {
        0..256 => (0, single_segment as usize),
        256..65792 => (1, 2),
        65792..0x1_0000_0000 => (2, 4),
        _ => (3, 8),
    };
    out.push(flag << 6 | (single_segment as u8) << 5);
    if !single_segment {
        out.push(((window_log - 10) << 3) as u8);
    }
    let stored = if flag == 1 { size - 256 } else { size };
    out.extend_from_slice(&stored.to_le_bytes()[..bytes]);
}

fn block_header(out: &mut [u8], last: bool, kind: u32, len: usize) {
    let bits = last as u32 | kind << 1 | (len as u32) << 3;
    out.copy_from_slice(&bits.to_le_bytes()[..3]);
}

pub(crate) fn encode(data: &[u8], prefix: &[u8], level: i32) -> Vec<u8> {
    encode_in_epochs(data, prefix, level, EPOCH)
}

fn encode_in_epochs(data: &[u8], prefix: &[u8], level: i32, epoch: usize) -> Vec<u8> {
    let prefix = &prefix[prefix.len().saturating_sub(1 << WINDOW_LOG_MAX)..];
    let params = Params::pick(level, data.len(), prefix.len());
    let window_size = 1usize << params.window_log;
    let single_segment = prefix.is_empty() && data.len() <= window_size;
    let mut out = Vec::with_capacity(data.len() + data.len() / BLOCK_MAX * 3 + 32);
    write_header(&mut out, data.len(), single_segment, params.window_log);
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0]);
        return out;
    }

    let long_len = if params.double {
        1 << params.long_log
    } else {
        0
    };
    let short_len = 1usize << params.short_log;
    SCRATCH.with_borrow_mut(|Scratch { tables, coder }| {
        coder.literals.reset();
        coder.sequences.reset();
        // large tables are made for the occasion and dropped afterwards
        let kept = long_len + short_len <= KEPT_ENTRIES && data.len() <= epoch;
        let mut fresh: (Vec<u32>, Vec<u32>);
        let (first, long, short) = if kept {
            let first = tables.claim(long_len, short_len, prefix.len() + data.len() + 1);
            (
                first,
                &mut tables.long[..long_len],
                &mut tables.short[..short_len],
            )
        } else {
            fresh = (vec![0; long_len], vec![0; short_len]);
            (1, &mut fresh.0[..], &mut fresh.1[..])
        };
        let mut matcher = Matcher {
            params,
            long,
            short,
            reps: [1, 4],
            max_distance: window_size,
        };

        let mut done = 0;
        // input bytes less output bytes over the blocks so far
        let mut saved = 0i64;
        while done < data.len() {
            // later epochs see the end of the previous one as their prefix
            let history = if done == 0 {
                prefix
            } else {
                matcher.long.fill(0);
                matcher.short.fill(0);
                &data[done - window_size.min(done)..done]
            };
            let part = &data[done..data.len().min(done + epoch)];
            let window = Window::new(history, part, first);
            matcher.load(&window);
            let mut at = 0;
            while at < part.len() {
                let mut len = BLOCK_MAX.min(part.len() - at);
                // only full blocks are split, and only once compression
                // has been seen to pay, so that noise stays in few blocks
                if len == BLOCK_MAX && saved >= 3 {
                    let block = &part[at..at + len];
                    len = if params.double {
                        split::by_chunks(block)
                    } else {
                        split::by_borders(block)
                    };
                }
                let block = &part[at..at + len];
                let ends = (done + at == 0, done + at + len == data.len());
                let before = out.len();
                coder.block(
                    &mut matcher,
                    &window,
                    window.split + at,
                    block,
                    ends,
                    &mut out,
                );
                saved += len as i64 - (out.len() - before) as i64;
                at += len;
            }
            done += part.len();
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::{SCRATCH, encode, encode_in_epochs};

    /// The kept tables are cleared when the index space runs out; frames
    /// made right before and after that point must both be sound.
    #[test]
    fn kept_tables_survive_index_wraparound() {
        let data: Vec<u8> = (0..60_000u32).flat_map(|i| (i / 7).to_le_bytes()).collect();
        let plain = encode(&data, &[], 3);
        SCRATCH.with_borrow_mut(|scratch| {
            scratch.tables.next = u32::MAX as usize - 2 * data.len();
        });
        for _ in 0..4 {
            let again = encode(&data, &[], 3);
            assert_eq!(again, plain);
            assert_eq!(crate::decode_all(&again).as_ref(), Ok(&data));
        }
        assert!(SCRATCH.with_borrow(|scratch| scratch.tables.next) < 1 << 30);
    }

    /// Epoch boundaries are far apart in real use; shrink them so that the
    /// hand-over of history from one epoch to the next is exercised.
    #[test]
    fn epochs_hand_history_over() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut data = Vec::new();
        while data.len() < 3 << 20 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let word = b"alpha beta gamma delta epsilon zeta eta theta ";
            let at = (state % 40) as usize;
            data.extend_from_slice(&word[at..at + (state >> 32) as usize % 6 + 1]);
        }
        let digests = [
            (
                1,
                "12064fe040632cfce8ee767f98c1ac590b89b257c41d3926bf61e8dc51900ca7",
            ),
            (
                3,
                "83ebb11544744e4bee41fb95c351a1b0aa8be457b7cec9f5c66ba285f3b433d3",
            ),
        ];
        for (level, verified) in digests {
            let packed = encode_in_epochs(&data, b"alpha beta gamma", level, 3 << 17);
            let ours = crate::decode_with_prefix(&packed, b"alpha beta gamma", data.len());
            assert_eq!(ours.as_ref(), Ok(&data));
            // the reference implementation decoded exactly these bytes
            // back to the input when the digests were recorded
            let digest = alt_hash::sha256(&packed);
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(hex, verified, "level {level}");
        }
    }
}
