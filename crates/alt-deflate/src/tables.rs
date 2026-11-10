//! Constant tables of the deflate format.

pub const MIN_MATCH: usize = 3;
pub const MAX_MATCH: usize = 258;

pub const LITERALS: usize = 256;
pub const END_BLOCK: usize = 256;
pub const LENGTH_CODES: usize = 29;
pub const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
pub const D_CODES: usize = 30;
pub const BL_CODES: usize = 19;
pub const HEAP_SIZE: usize = 2 * L_CODES + 1;
pub const MAX_BITS: usize = 15;
pub const MAX_BL_BITS: u8 = 7;

pub const REP_3_6: usize = 16;
pub const REPZ_3_10: usize = 17;
pub const REPZ_11_138: usize = 18;

pub const EXTRA_LBITS: [u8; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
pub const EXTRA_DBITS: [u8; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
pub const EXTRA_BLBITS: [u8; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// order in which the bit length code lengths are sent
pub const BL_ORDER: [u8; BL_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

const LENGTH_TABLES: ([u8; 256], [u8; LENGTH_CODES]) = length_tables();
/// length code for each match length minus MIN_MATCH
pub static LENGTH_CODE: [u8; 256] = LENGTH_TABLES.0;
/// first match length minus MIN_MATCH of each length code
pub static BASE_LENGTH: [u8; LENGTH_CODES] = LENGTH_TABLES.1;

const DIST_TABLES: ([u8; 512], [u16; D_CODES]) = dist_tables();
static DIST_CODE: [u8; 512] = DIST_TABLES.0;
/// first distance minus one of each distance code
pub static BASE_DIST: [u16; D_CODES] = DIST_TABLES.1;

pub static STATIC_LLEN: [u8; L_CODES + 2] = static_literal_lengths();
pub static STATIC_LCODE: [u16; L_CODES + 2] = static_literal_codes();
pub static STATIC_DLEN: [u8; D_CODES] = [5; D_CODES];
pub static STATIC_DCODE: [u16; D_CODES] = static_distance_codes();

/// distance code of a match distance minus one
pub fn dist_code(dist: usize) -> usize {
    let slot = if dist < 256 { dist } else { 256 + (dist >> 7) };
    DIST_CODE[slot] as usize
}

const fn length_tables() -> ([u8; 256], [u8; LENGTH_CODES]) {
    let mut code_of = [0u8; 256];
    let mut base = [0u8; LENGTH_CODES];
    let mut length = 0;
    let mut code = 0;
    while code < LENGTH_CODES - 1 {
        base[code] = length as u8;
        let mut n = 0;
        while n < (1 << EXTRA_LBITS[code]) {
            code_of[length] = code as u8;
            length += 1;
            n += 1;
        }
        code += 1;
    }
    // length 258 has its own code without extra bits
    code_of[255] = code as u8;
    (code_of, base)
}

const fn dist_tables() -> ([u8; 512], [u16; D_CODES]) {
    let mut code_of = [0u8; 512];
    let mut base = [0u16; D_CODES];
    let mut dist = 0;
    let mut code = 0;
    while code < 16 {
        base[code] = dist as u16;
        let mut n = 0;
        while n < (1 << EXTRA_DBITS[code]) {
            code_of[dist] = code as u8;
            dist += 1;
            n += 1;
        }
        code += 1;
    }
    // the upper half of the table is indexed by distance / 128
    dist >>= 7;
    while code < D_CODES {
        base[code] = (dist << 7) as u16;
        let mut n = 0;
        while n < (1 << (EXTRA_DBITS[code] - 7)) {
            code_of[256 + dist] = code as u8;
            dist += 1;
            n += 1;
        }
        code += 1;
    }
    (code_of, base)
}

const fn static_literal_lengths() -> [u8; L_CODES + 2] {
    let mut lens = [8u8; L_CODES + 2];
    let mut n = 144;
    while n < 256 {
        lens[n] = 9;
        n += 1;
    }
    while n < 280 {
        lens[n] = 7;
        n += 1;
    }
    lens
}

const fn static_literal_codes() -> [u16; L_CODES + 2] {
    let mut codes = [0u16; L_CODES + 2];
    assign_codes(&static_literal_lengths(), &mut codes);
    codes
}

const fn static_distance_codes() -> [u16; D_CODES] {
    let mut codes = [0u16; D_CODES];
    assign_codes(&[5; D_CODES], &mut codes);
    codes
}

/// Assigns canonical Huffman codes, already bit-reversed for LSB-first output.
pub const fn assign_codes(lens: &[u8], codes: &mut [u16]) {
    let mut count = [0u32; MAX_BITS + 1];
    let mut n = 0;
    while n < lens.len() {
        count[lens[n] as usize] += 1;
        n += 1;
    }
    count[0] = 0;
    let mut next = [0u32; MAX_BITS + 1];
    let mut code = 0;
    let mut bits = 1;
    while bits <= MAX_BITS {
        code = (code + count[bits - 1]) << 1;
        next[bits] = code;
        bits += 1;
    }
    n = 0;
    while n < lens.len() {
        let len = lens[n] as usize;
        if len != 0 {
            codes[n] = (next[len].reverse_bits() >> (32 - len)) as u16;
            next[len] += 1;
        }
        n += 1;
    }
}
