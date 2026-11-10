//! One deflate block: symbol collection and the choice between stored,
//! fixed-code and dynamic-code output.

use crate::bits::BitWriter;
use crate::tables::{
    BASE_DIST, BASE_LENGTH, BL_CODES, BL_ORDER, END_BLOCK, EXTRA_DBITS, EXTRA_LBITS, LENGTH_CODE,
    LITERALS, MIN_MATCH, REP_3_6, REPZ_3_10, REPZ_11_138, STATIC_DCODE, STATIC_DLEN, STATIC_LCODE,
    STATIC_LLEN, dist_code,
};
use crate::trees::{BIT_LENGTH, Cost, DISTANCE, LITERAL, Tree, build};

/// a block is flushed when it holds this many symbols, as in zlib with memLevel 8
const SYMBOL_LIMIT: usize = (1 << 14) - 1;

const STORED: u32 = 0;
const FIXED: u32 = 1;
const DYNAMIC: u32 = 2;

pub struct Block {
    /// per symbol: the match distance in the upper half (0 for a literal), and the
    /// literal byte or the match length minus MIN_MATCH in the lower
    symbols: Vec<u32>,
    literals: Tree,
    distances: Tree,
    bit_lengths: Tree,
}

impl Block {
    pub fn new() -> Self {
        let mut block = Self {
            symbols: Vec::with_capacity(SYMBOL_LIMIT + 8),
            literals: Tree::new(),
            distances: Tree::new(),
            bit_lengths: Tree::new(),
        };
        block.reset();
        block
    }

    fn reset(&mut self) {
        self.symbols.clear();
        self.literals.freq.fill(0);
        self.distances.freq.fill(0);
        self.bit_lengths.freq.fill(0);
        self.literals.freq[END_BLOCK] = 1;
    }

    /// Returns true when the block is full and must be flushed.
    pub fn tally_literal(&mut self, byte: u8) -> bool {
        self.symbols.push(u32::from(byte));
        self.literals.freq[byte as usize] += 1;
        self.symbols.len() == SYMBOL_LIMIT
    }

    /// Returns true when the block is full and must be flushed.
    pub fn tally_match(&mut self, dist: usize, len: usize) -> bool {
        let lc = len - MIN_MATCH;
        self.symbols.push((dist as u32) << 16 | lc as u32);
        self.literals.freq[LENGTH_CODE[lc] as usize + LITERALS + 1] += 1;
        self.distances.freq[dist_code(dist - 1)] += 1;
        self.symbols.len() == SYMBOL_LIMIT
    }

    /// Writes the block in its smallest encoding. `raw` is the input the
    /// symbols stand for, or `None` when it is no longer available.
    pub fn flush(&mut self, out: &mut BitWriter, raw: Option<&[u8]>, last: bool) {
        let mut cost = Cost::default();
        build(&mut self.literals, &LITERAL, &mut cost);
        build(&mut self.distances, &DISTANCE, &mut cost);
        let bl_codes = self.build_bit_lengths(&mut cost);

        let fixed_bytes = (cost.fixed + 3 + 7) >> 3;
        let best_bytes = fixed_bytes.min((cost.dynamic + 3 + 7) >> 3);
        let last_bit = u32::from(last);

        match raw {
            Some(raw) if raw.len() as u64 + 4 <= best_bytes => {
                out.put((STORED << 1) + last_bit, 3);
                out.align();
                let len = raw.len() as u16;
                out.put_bytes(&len.to_le_bytes());
                out.put_bytes(&(!len).to_le_bytes());
                out.put_bytes(raw);
            }
            // a tie goes to the fixed codes
            _ if fixed_bytes == best_bytes => {
                out.put((FIXED << 1) + last_bit, 3);
                let literals = (&STATIC_LCODE[..], &STATIC_LLEN[..]);
                let distances = (&STATIC_DCODE[..], &STATIC_DLEN[..]);
                self.write_symbols(out, literals, distances);
            }
            _ => {
                out.put((DYNAMIC << 1) + last_bit, 3);
                self.write_trees(out, bl_codes);
                let literals = (&self.literals.code[..], &self.literals.len[..]);
                let distances = (&self.distances.code[..], &self.distances.len[..]);
                self.write_symbols(out, literals, distances);
            }
        }
        self.reset();
        if last {
            out.align();
        }
    }

    /// Returns how many bit length code lengths have to be sent.
    fn build_bit_lengths(&mut self, cost: &mut Cost) -> usize {
        let freq = &mut self.bit_lengths.freq;
        let literals = &self.literals;
        let distances = &self.distances;
        code_length_runs(&literals.len[..=literals.max_code], |symbol, _, _| {
            freq[symbol] += 1;
        });
        code_length_runs(&distances.len[..=distances.max_code], |symbol, _, _| {
            freq[symbol] += 1;
        });
        build(&mut self.bit_lengths, &BIT_LENGTH, cost);

        let mut count = BL_CODES;
        while count > 4 && self.bit_lengths.len[BL_ORDER[count - 1] as usize] == 0 {
            count -= 1;
        }
        cost.dynamic = cost.dynamic.wrapping_add(3 * count as u64 + 5 + 5 + 4);
        count
    }

    fn write_trees(&self, out: &mut BitWriter, bl_codes: usize) {
        let (literals, distances, bit_lengths) =
            (&self.literals, &self.distances, &self.bit_lengths);
        out.put((literals.max_code + 1 - 257) as u32, 5);
        out.put(distances.max_code as u32, 5);
        out.put((bl_codes - 4) as u32, 4);
        for &symbol in &BL_ORDER[..bl_codes] {
            out.put(u32::from(bit_lengths.len[symbol as usize]), 3);
        }
        let mut send = |symbol: usize, extra: u32, extra_bits: u32| {
            out.put(
                u32::from(bit_lengths.code[symbol]),
                u32::from(bit_lengths.len[symbol]),
            );
            if extra_bits != 0 {
                out.put(extra, extra_bits);
            }
        };
        code_length_runs(&literals.len[..=literals.max_code], &mut send);
        code_length_runs(&distances.len[..=distances.max_code], &mut send);
    }

    fn write_symbols(
        &self,
        out: &mut BitWriter,
        (lcode, llen): (&[u16], &[u8]),
        (dcode, dlen): (&[u16], &[u8]),
    ) {
        // a symbol is at most 48 bits: a length and a distance with their extra bits
        out.reserve(self.symbols.len() * 6 + 8);
        for symbol in &self.symbols {
            let (dist, lc) = (symbol >> 16, (symbol & 0xffff) as usize);
            // room for every symbol of the block was reserved above
            unsafe {
                if dist == 0 {
                    out.put_reserved(u32::from(lcode[lc]), u32::from(llen[lc]));
                    continue;
                }
                // a code and its extra bits go out together: at most 20 and 28 bits;
                // the longest length has its own code, whose base the table leaves at zero
                let code = LENGTH_CODE[lc] as usize;
                let symbol = code + LITERALS + 1;
                let bits = u32::from(llen[symbol]);
                let extra_bits = u32::from(EXTRA_LBITS[code]);
                let extra = (lc - BASE_LENGTH[code] as usize) as u32 & ((1 << extra_bits) - 1);
                out.put_reserved(u32::from(lcode[symbol]) | extra << bits, bits + extra_bits);
                let dist = dist as usize - 1;
                let code = dist_code(dist);
                let bits = u32::from(dlen[code]);
                let extra_bits = u32::from(EXTRA_DBITS[code]);
                let extra = (dist - BASE_DIST[code] as usize) as u32;
                out.put_reserved(u32::from(dcode[code]) | extra << bits, bits + extra_bits);
            }
        }
        out.put(u32::from(lcode[END_BLOCK]), u32::from(llen[END_BLOCK]));
    }
}

/// Run-length encodes a sequence of code lengths into bit length symbols,
/// calling `emit(symbol, extra value, extra bit count)` for each.
fn code_length_runs(lens: &[u8], mut emit: impl FnMut(usize, u32, u32)) {
    let mut prev = None;
    let mut count = 0u32;
    let (mut max_count, mut min_count) = if lens[0] == 0 { (138, 3) } else { (7, 4) };
    for (n, &cur) in lens.iter().enumerate() {
        let next = lens.get(n + 1).copied();
        count += 1;
        if count < max_count && Some(cur) == next {
            continue;
        }
        if count < min_count {
            for _ in 0..count {
                emit(cur as usize, 0, 0);
            }
        } else if cur != 0 {
            if Some(cur) != prev {
                emit(cur as usize, 0, 0);
                count -= 1;
            }
            emit(REP_3_6, count - 3, 2);
        } else if count <= 10 {
            emit(REPZ_3_10, count - 3, 3);
        } else {
            emit(REPZ_11_138, count - 11, 7);
        }
        count = 0;
        prev = Some(cur);
        (max_count, min_count) = if next == Some(0) {
            (138, 3)
        } else if Some(cur) == next {
            (6, 3)
        } else {
            (7, 4)
        };
    }
}
