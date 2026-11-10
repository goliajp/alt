//! Huffman decoding tables.
//!
//! A table is indexed by the next bits of the input, lowest bit first. Codes no longer
//! than the main index are answered directly; a longer code's entry points at a
//! second-level table indexed by the bits that follow.

use crate::Error;

/// Entry layout: bits 0..6 the code bits to consume, bits 8..13 the count of extra bits
/// (or, for a pointer, the index width of the second-level table), bits 16..32 the
/// literal, the base length or distance, or the offset of the second-level table.
pub(crate) const LITERAL: u32 = 1 << 15;
/// end of block when the value is zero, a code that the block does not define otherwise
pub(crate) const SPECIAL: u32 = 1 << 14;
pub(crate) const POINTER: u32 = 1 << 13;
const UNDEFINED: u32 = SPECIAL | 1 << 16;

pub(crate) const LITLEN_BITS: u32 = 11;
pub(crate) const DISTANCE_BITS: u32 = 8;
pub(crate) const PRECODE_BITS: u32 = 7;
/// the most entries a complete code of this many symbols can need
pub(crate) const LITLEN_ENTRIES: usize = 2342;
pub(crate) const DISTANCE_ENTRIES: usize = 402;
pub(crate) const PRECODE_ENTRIES: usize = 128;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    /// the code lengths of the two codes below
    Precode,
    LiteralLength,
    Distance,
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// what decoding `symbol` yields, without the count of code bits
fn meaning(kind: Kind, symbol: usize) -> u32 {
    match kind {
        Kind::Precode => (symbol as u32) << 16,
        Kind::LiteralLength => match symbol {
            0..256 => LITERAL | (symbol as u32) << 16,
            256 => SPECIAL,
            257..286 => {
                (LENGTH_BASE[symbol - 257] as u32) << 16 | (LENGTH_EXTRA[symbol - 257] as u32) << 8
            }
            _ => UNDEFINED,
        },
        Kind::Distance => match symbol {
            0..30 => (DISTANCE_BASE[symbol] as u32) << 16 | (DISTANCE_EXTRA[symbol] as u32) << 8,
            _ => UNDEFINED,
        },
    }
}

/// Builds the table for the canonical code with these lengths and returns the width
/// of its first-level index: the longest code, but at most `widest`, so that a block
/// of short codes does not pay for a large table. A code set that leaves bit patterns
/// unused is only accepted in the forms zlib accepts: no codes at all, or a single
/// one-bit code.
pub(crate) fn build(
    table: &mut [u32],
    lengths: &[u8],
    kind: Kind,
    widest: u32,
) -> Result<u32, Error> {
    let mut count = [0u16; 16];
    for length in lengths {
        count[*length as usize] += 1;
    }
    let Some(longest) = (1..16).rev().find(|length| count[*length] != 0) else {
        // a block without any code of this kind: every use of it is an error
        table[..2].fill(UNDEFINED | 1);
        return Ok(1);
    };
    let main_bits = widest.min(longest as u32);
    let main = 1usize << main_bits;
    let mut unused = 1i32;
    for codes in &count[1..] {
        unused = (unused << 1) - *codes as i32;
        if unused < 0 {
            return Err(Error::Corrupt("more codes than their lengths allow"));
        }
    }
    if unused > 0 && (kind == Kind::Precode || longest != 1) {
        return Err(Error::Corrupt("incomplete set of codes"));
    }
    if unused > 0 {
        // a complete code writes every entry itself
        table[..main].fill(UNDEFINED | 1);
    }

    // symbols ordered by code length, then by value
    let mut start = [0u16; 16];
    for length in 1..15 {
        start[length + 1] = start[length] + count[length];
    }
    let mut ordered = [0u16; 288];
    for (symbol, length) in lengths.iter().enumerate() {
        if *length != 0 {
            ordered[start[*length as usize] as usize] = symbol as u16;
            start[*length as usize] += 1;
        }
    }

    let used = lengths.len() - count[0] as usize;
    let mut next_free = main;
    let mut code = 0u32;
    let mut length = (1..16).find(|length| count[*length] != 0).unwrap();
    // the second-level table being filled: its first-level index, offset and width
    let mut second: Option<(usize, usize, u32)> = None;
    for symbol in &ordered[..used] {
        let reversed = (code.reverse_bits() >> (32 - length)) as usize;
        let entry = meaning(kind, *symbol as usize);
        if length as u32 <= main_bits {
            for slot in (reversed..main).step_by(1 << length) {
                table[slot] = entry | length as u32;
            }
        } else {
            let prefix = reversed & (main - 1);
            let (offset, width) = match second {
                Some((index, offset, width)) if index == prefix => (offset, width),
                _ => {
                    // wide enough for this code and for the longer ones that still fit
                    let mut width = length as u32 - main_bits;
                    let mut room = 1i32 << width;
                    while (width + main_bits) < longest as u32 {
                        room -= count[(width + main_bits) as usize] as i32;
                        if room <= 0 {
                            break;
                        }
                        width += 1;
                        room <<= 1;
                    }
                    let offset = next_free;
                    next_free += 1 << width;
                    table[prefix] = POINTER | (offset as u32) << 16 | width << 8 | main_bits;
                    second = Some((prefix, offset, width));
                    (offset, width)
                }
            };
            let rest = length as u32 - main_bits;
            for slot in ((reversed >> main_bits)..1 << width).step_by(1 << rest) {
                table[offset + slot] = entry | rest;
            }
        }
        code += 1;
        count[length] -= 1;
        while count[length] == 0 && length < longest {
            length += 1;
            code <<= 1;
        }
    }
    Ok(main_bits)
}
