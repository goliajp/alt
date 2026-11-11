//! Several chunks or parent nodes compressed side by side, one per vector lane.
//!
//! The sixteen state words become sixteen vectors, each holding the same word of
//! every lane, so the rounds are the scalar ones applied to all lanes at once. The
//! message has to be laid out the same way: word `k` of every lane in one vector.

use super::{Backend, IV, SCHEDULE};

/// equal-sized nodes that lie one after another in memory
pub(super) struct Batch<'a> {
    pub key: &'a [u32; 8],
    /// bytes in one node, a multiple of 64: a chunk or the two children of a parent
    pub node_len: usize,
    /// whether each node counts one more than the one before, as chunks do
    pub increment: bool,
    /// flags of every block, of the first block and of the last block of a node
    pub flags: u32,
    pub flags_start: u32,
    pub flags_end: u32,
}

impl Batch<'_> {
    /// How many whole nodes `len` bytes hold. A shift: the length of a node is a
    /// power of two, and a division here would cost as much as half a block.
    #[inline(always)]
    pub fn nodes(&self, len: usize) -> usize {
        debug_assert!(self.node_len.is_power_of_two());
        len >> self.node_len.trailing_zeros()
    }
}

/// a vector of one 32-bit word per lane
pub(super) trait Lanes: Copy {
    const N: usize;
    unsafe fn splat(word: u32) -> Self;
    /// the first `N` of `words`, one per lane
    unsafe fn words(words: &[u32; 8]) -> Self;
    unsafe fn add(self, other: Self) -> Self;
    unsafe fn xor(self, other: Self) -> Self;
    unsafe fn rot16(self) -> Self;
    unsafe fn rot12(self) -> Self;
    unsafe fn rot8(self) -> Self;
    unsafe fn rot7(self) -> Self;
    /// One 64-byte block from each lane, lanes `stride` bytes apart, as sixteen
    /// vectors of which number `k` holds word `k` of every lane. Reads 64 bytes at
    /// `blocks + lane * stride` for each of the `N` lanes.
    unsafe fn load(blocks: *const u8, stride: usize) -> [Self; 16];
    /// writes the eight words of each lane as 32 consecutive bytes, lane after lane:
    /// `N * 32` bytes at `out`
    unsafe fn store(h: &[Self; 8], out: *mut u8);
}

macro_rules! g {
    ($v:ident, $a:literal, $b:literal, $c:literal, $d:literal, $x:expr, $y:expr) => {
        $v[$a] = $v[$a].add($v[$b]).add($x);
        $v[$d] = $v[$d].xor($v[$a]).rot16();
        $v[$c] = $v[$c].add($v[$d]);
        $v[$b] = $v[$b].xor($v[$c]).rot12();
        $v[$a] = $v[$a].add($v[$b]).add($y);
        $v[$d] = $v[$d].xor($v[$a]).rot8();
        $v[$c] = $v[$c].add($v[$d]);
        $v[$b] = $v[$b].xor($v[$c]).rot7();
    };
}

#[inline(always)]
unsafe fn round<V: Lanes>(v: &mut [V; 16], m: &[V; 16], s: &[usize; 16]) {
    unsafe {
        g!(v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        g!(v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        g!(v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        g!(v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        g!(v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        g!(v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        g!(v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        g!(v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }
}

/// Compresses the `V::N` nodes in `input`, the first of which counts `counter`, and
/// writes their chaining values to `out`. The caller has detected the CPU features
/// `V` needs.
#[inline(always)]
unsafe fn compress_lanes<V: Lanes>(batch: &Batch, input: &[u8], counter: u64, out: &mut [u8]) {
    assert!(input.len() == V::N * batch.node_len && out.len() == V::N * 32);
    let (mut low, mut high) = ([0; 8], [0; 8]);
    for lane in 0..8 {
        let count = counter + if batch.increment { lane as u64 } else { 0 };
        low[lane] = count as u32;
        high[lane] = (count >> 32) as u32;
    }
    let blocks = batch.node_len / 64;
    // every load stays inside `input` and the store inside `out`, whose lengths were
    // just checked; unaligned access is what the vector loads and stores are for
    unsafe {
        let key = batch.key;
        let mut h = [
            V::splat(key[0]),
            V::splat(key[1]),
            V::splat(key[2]),
            V::splat(key[3]),
            V::splat(key[4]),
            V::splat(key[5]),
            V::splat(key[6]),
            V::splat(key[7]),
        ];
        let iv = [
            V::splat(IV[0]),
            V::splat(IV[1]),
            V::splat(IV[2]),
            V::splat(IV[3]),
        ];
        let (low, high, block_len) = (V::words(&low), V::words(&high), V::splat(64));
        let mut flags = batch.flags | batch.flags_start;
        for block in 0..blocks {
            if block + 1 == blocks {
                flags |= batch.flags_end;
            }
            let m = V::load(input.as_ptr().add(block * 64), batch.node_len);
            let mut v = [
                h[0],
                h[1],
                h[2],
                h[3],
                h[4],
                h[5],
                h[6],
                h[7],
                iv[0],
                iv[1],
                iv[2],
                iv[3],
                low,
                high,
                block_len,
                V::splat(flags),
            ];
            round(&mut v, &m, &SCHEDULE[0]);
            round(&mut v, &m, &SCHEDULE[1]);
            round(&mut v, &m, &SCHEDULE[2]);
            round(&mut v, &m, &SCHEDULE[3]);
            round(&mut v, &m, &SCHEDULE[4]);
            round(&mut v, &m, &SCHEDULE[5]);
            round(&mut v, &m, &SCHEDULE[6]);
            for word in 0..8 {
                h[word] = v[word].xor(v[word + 8]);
            }
            flags = batch.flags;
        }
        V::store(&h, out.as_mut_ptr());
    }
}

/// Compresses as many whole groups of `V::N` nodes as `input` holds and returns
/// what is left of the input and of `out`, and the counter of the next node. The
/// caller has detected the CPU features `V` needs.
#[inline(always)]
pub(super) unsafe fn in_groups<'a, 'b, V: Lanes>(
    batch: &Batch,
    input: &'a [u8],
    mut counter: u64,
    out: &'b mut [u8],
) -> (&'a [u8], u64, &'b mut [u8]) {
    let group_len = V::N * batch.node_len;
    let (mut input, mut out) = (input, out);
    while input.len() >= group_len {
        let (group, after) = input.split_at(group_len);
        let (group_out, after_out) = out.split_at_mut(V::N * 32);
        // the caller vouches for the features
        unsafe { compress_lanes::<V>(batch, group, counter, group_out) };
        counter += V::N as u64 * batch.increment as u64;
        (input, out) = (after, after_out);
    }
    (input, counter, out)
}

pub(super) fn cv_bytes(words: &[u32; 8]) -> [u8; 32] {
    let mut bytes = [0; 32];
    for (four, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        *four = word.to_le_bytes();
    }
    bytes
}

/// compresses the nodes in `input` one after another, block by block
pub(super) fn one_by_one(
    batch: &Batch,
    input: &[u8],
    mut counter: u64,
    out: &mut [u8],
    backend: Backend,
) {
    let (mut input, mut out) = (input, out);
    while input.len() >= batch.node_len {
        let (node, after) = input.split_at(batch.node_len);
        let (node_out, after_out) = out.split_first_chunk_mut::<32>().unwrap();
        let mut cv = *batch.key;
        let (last, before) = node.as_chunks::<64>().0.split_last().unwrap();
        backend.absorb(&mut cv, before, counter, batch.flags, batch.flags_start);
        let start = if before.is_empty() {
            batch.flags_start
        } else {
            0
        };
        let flags = batch.flags | start | batch.flags_end;
        *node_out = cv_bytes(&backend.compress(&cv, last, 64, counter, flags));
        counter += batch.increment as u64;
        (input, out) = (after, after_out);
    }
}
