//! the compression function in plain integer arithmetic

use super::{IV, SCHEDULE};

#[inline(always)]
fn g(v: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, x: u32, y: u32) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(12);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(8);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(7);
}

#[inline(always)]
fn round(v: &mut [u32; 16], m: &[u32; 16], s: &[usize; 16]) {
    g(v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
    g(v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
    g(v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
    g(v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
    g(v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
    g(v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
    g(v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
    g(v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
}

#[inline(always)]
fn mix(cv: &[u32; 8], block: &[u8; 64], block_len: u32, counter: u64, flags: u32) -> [u32; 8] {
    let mut m = [0u32; 16];
    for (word, bytes) in m.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*bytes);
    }
    let mut v = [
        cv[0],
        cv[1],
        cv[2],
        cv[3],
        cv[4],
        cv[5],
        cv[6],
        cv[7],
        IV[0],
        IV[1],
        IV[2],
        IV[3],
        counter as u32,
        (counter >> 32) as u32,
        block_len,
        flags,
    ];
    round(&mut v, &m, &SCHEDULE[0]);
    round(&mut v, &m, &SCHEDULE[1]);
    round(&mut v, &m, &SCHEDULE[2]);
    round(&mut v, &m, &SCHEDULE[3]);
    round(&mut v, &m, &SCHEDULE[4]);
    round(&mut v, &m, &SCHEDULE[5]);
    round(&mut v, &m, &SCHEDULE[6]);
    let mut out = [0; 8];
    for (index, word) in out.iter_mut().enumerate() {
        *word = v[index] ^ v[index + 8];
    }
    out
}

/// The reference implementation every accelerated one is compared with: the chaining
/// value that follows `cv` after one block, of which `block_len` bytes are input.
pub fn compress(
    cv: &[u32; 8],
    block: &[u8; 64],
    block_len: u32,
    counter: u64,
    flags: u32,
) -> [u32; 8] {
    mix(cv, block, block_len, counter, flags)
}

/// Advances `cv` over whole blocks of one chunk; the first one also gets
/// `flags_start`. One loop, so the chaining value stays in registers between blocks
/// instead of going through memory, which would add to every block's latency.
pub fn absorb(cv: &mut [u32; 8], blocks: &[[u8; 64]], counter: u64, flags: u32, flags_start: u32) {
    let mut state = *cv;
    let mut block_flags = flags | flags_start;
    for block in blocks {
        state = mix(&state, block, 64, counter, block_flags);
        block_flags = flags;
    }
    *cv = state;
}
