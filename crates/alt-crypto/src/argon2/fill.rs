//! The memory-filling loop: block addressing and the compression function G.

pub(super) type Block = [u64; 128];

pub(super) struct Layout {
    pub lanes: usize,
    pub lane_len: usize,
    pub segment: usize,
    pub passes: u32,
}

/// fills one segment of one lane
pub(super) fn segment(memory: &mut [Block], layout: &Layout, pass: u32, slice: usize, lane: usize) {
    // argon2id addresses the first half of the first pass independently of the data
    let independent = pass == 0 && slice < 2;
    let mut input = [0u64; 128];
    let mut addresses = [0u64; 128];
    if independent {
        input[..6].copy_from_slice(&[
            u64::from(pass),
            lane as u64,
            slice as u64,
            (layout.lanes * layout.lane_len) as u64,
            u64::from(layout.passes),
            2,
        ]);
    }
    // the first two blocks of every lane come from the seed
    let first = if pass == 0 && slice == 0 { 2 } else { 0 };
    if independent && first != 0 {
        next_addresses(&mut input, &mut addresses);
    }
    let base = lane * layout.lane_len;
    let offset = slice * layout.segment;
    for index in first..layout.segment {
        let current = base + offset + index;
        let previous = if offset + index == 0 {
            current + layout.lane_len - 1
        } else {
            current - 1
        };
        let random = if independent {
            if index % 128 == 0 {
                next_addresses(&mut input, &mut addresses);
            }
            addresses[index % 128]
        } else {
            memory[previous][0]
        };
        let same_lane_only = pass == 0 && slice == 0;
        let reference_lane = if same_lane_only {
            lane
        } else {
            (random >> 32) as usize % layout.lanes
        };
        let same_lane = reference_lane == lane;
        // blocks finished so far that this one may refer to
        let area = if pass == 0 {
            if same_lane {
                offset + index - 1
            } else {
                offset - usize::from(index == 0)
            }
        } else if same_lane {
            layout.lane_len - layout.segment + index - 1
        } else {
            layout.lane_len - layout.segment - usize::from(index == 0)
        } as u64;
        let j1 = random & 0xffff_ffff;
        let skew = (j1 * j1) >> 32;
        let back = area - 1 - ((area * skew) >> 32);
        let start = if pass == 0 || slice == 3 {
            0
        } else {
            offset + layout.segment
        };
        let reference =
            reference_lane * layout.lane_len + (start + back as usize) % layout.lane_len;

        let mut mixed = memory[previous];
        for (word, other) in mixed.iter_mut().zip(&memory[reference]) {
            *word ^= other;
        }
        let mut out = mixed;
        permute(&mut out);
        let target = &mut memory[current];
        if pass == 0 {
            for i in 0..128 {
                target[i] = out[i] ^ mixed[i];
            }
        } else {
            for i in 0..128 {
                target[i] ^= out[i] ^ mixed[i];
            }
        }
    }
}

/// the next 128 data-independent addresses: G(0, G(0, input)) with the counter advanced
fn next_addresses(input: &mut Block, addresses: &mut Block) {
    input[6] += 1;
    *addresses = *input;
    for _ in 0..2 {
        let before = *addresses;
        permute(addresses);
        for (word, other) in addresses.iter_mut().zip(before) {
            *word ^= other;
        }
    }
}

macro_rules! mix {
    ($a:ident, $b:ident, $c:ident, $d:ident) => {
        $a = multiply_add($a, $b);
        $d = ($d ^ $a).rotate_right(32);
        $c = multiply_add($c, $d);
        $b = ($b ^ $c).rotate_right(24);
        $a = multiply_add($a, $b);
        $d = ($d ^ $a).rotate_right(16);
        $c = multiply_add($c, $d);
        $b = ($b ^ $c).rotate_right(63);
    };
}

/// the BLAKE2b round with multiplications, over sixteen words of the block
macro_rules! round {
    ($v:ident, $($i:expr),+) => {{
        let [mut v0, mut v1, mut v2, mut v3, mut v4, mut v5, mut v6, mut v7,
             mut v8, mut v9, mut v10, mut v11, mut v12, mut v13, mut v14, mut v15] = [$($v[$i]),+];
        mix!(v0, v4, v8, v12);
        mix!(v1, v5, v9, v13);
        mix!(v2, v6, v10, v14);
        mix!(v3, v7, v11, v15);
        mix!(v0, v5, v10, v15);
        mix!(v1, v6, v11, v12);
        mix!(v2, v7, v8, v13);
        mix!(v3, v4, v9, v14);
        let after = [v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15];
        for (at, word) in [$($i),+].into_iter().zip(after) {
            $v[at] = word;
        }
    }};
}

/// a + b + 2 * low32(a) * low32(b)
#[inline(always)]
fn multiply_add(a: u64, b: u64) -> u64 {
    let product = (a & 0xffff_ffff) * (b & 0xffff_ffff);
    a.wrapping_add(b).wrapping_add(product << 1)
}

/// the permutation P applied to the eight rows, then the eight columns
fn permute(v: &mut Block) {
    for row in 0..8 {
        let r = 16 * row;
        round!(
            v,
            r,
            r + 1,
            r + 2,
            r + 3,
            r + 4,
            r + 5,
            r + 6,
            r + 7,
            r + 8,
            r + 9,
            r + 10,
            r + 11,
            r + 12,
            r + 13,
            r + 14,
            r + 15
        );
    }
    for column in 0..8 {
        let c = 2 * column;
        round!(
            v,
            c,
            c + 1,
            c + 16,
            c + 17,
            c + 32,
            c + 33,
            c + 48,
            c + 49,
            c + 64,
            c + 65,
            c + 80,
            c + 81,
            c + 96,
            c + 97,
            c + 112,
            c + 113
        );
    }
}
