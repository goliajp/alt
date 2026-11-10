//! Huffman tree construction with zlib's tie-breaking and length limiting.

use crate::tables::{
    BL_CODES, D_CODES, EXTRA_BLBITS, EXTRA_DBITS, EXTRA_LBITS, HEAP_SIZE, L_CODES, LITERALS,
    MAX_BITS, MAX_BL_BITS, STATIC_DLEN, STATIC_LLEN, assign_codes,
};

pub struct Tree {
    pub freq: [u16; HEAP_SIZE],
    pub len: [u8; HEAP_SIZE],
    pub code: [u16; HEAP_SIZE],
    parent: [u16; HEAP_SIZE],
    /// largest symbol with a code
    pub max_code: usize,
}

impl Tree {
    pub fn new() -> Self {
        Self {
            freq: [0; HEAP_SIZE],
            len: [0; HEAP_SIZE],
            code: [0; HEAP_SIZE],
            parent: [0; HEAP_SIZE],
            max_code: 0,
        }
    }
}

/// What distinguishes the literal, distance and bit length alphabets.
pub struct Alphabet {
    fixed_len: Option<&'static [u8]>,
    extra: &'static [u8],
    extra_base: usize,
    symbols: usize,
    max_len: u8,
}

pub const LITERAL: Alphabet = Alphabet {
    fixed_len: Some(&STATIC_LLEN),
    extra: &EXTRA_LBITS,
    extra_base: LITERALS + 1,
    symbols: L_CODES,
    max_len: MAX_BITS as u8,
};

pub const DISTANCE: Alphabet = Alphabet {
    fixed_len: Some(&STATIC_DLEN),
    extra: &EXTRA_DBITS,
    extra_base: 0,
    symbols: D_CODES,
    max_len: MAX_BITS as u8,
};

pub const BIT_LENGTH: Alphabet = Alphabet {
    fixed_len: None,
    extra: &EXTRA_BLBITS,
    extra_base: 0,
    symbols: BL_CODES,
    max_len: MAX_BL_BITS,
};

/// Block size in bits with dynamic and with fixed codes.
///
/// Wrapping arithmetic: forcing a second code into a one-symbol tree
/// subtracts before the lengths are added.
#[derive(Default)]
pub struct Cost {
    pub dynamic: u64,
    pub fixed: u64,
}

/// A min-heap in the low slots; the high slots collect the nodes in the
/// order they leave the heap, which is the order lengths are assigned in.
struct Heap {
    slots: [u16; HEAP_SIZE],
    len: usize,
    depth: [u8; HEAP_SIZE],
}

impl Heap {
    fn push_unordered(&mut self, node: usize) {
        self.len += 1;
        self.slots[self.len] = node as u16;
    }

    /// equal frequencies are ordered by subtree depth, shallower first
    fn before(&self, freq: &[u16; HEAP_SIZE], n: usize, m: usize) -> bool {
        freq[n] < freq[m] || (freq[n] == freq[m] && self.depth[n] <= self.depth[m])
    }

    fn sift_down(&mut self, freq: &[u16; HEAP_SIZE], mut k: usize) {
        let node = self.slots[k];
        let mut child = k << 1;
        while child <= self.len {
            if child < self.len
                && self.before(
                    freq,
                    self.slots[child + 1] as usize,
                    self.slots[child] as usize,
                )
            {
                child += 1;
            }
            if self.before(freq, node as usize, self.slots[child] as usize) {
                break;
            }
            self.slots[k] = self.slots[child];
            k = child;
            child <<= 1;
        }
        self.slots[k] = node;
    }

    fn pop(&mut self, freq: &[u16; HEAP_SIZE]) -> usize {
        let top = self.slots[1];
        self.slots[1] = self.slots[self.len];
        self.len -= 1;
        self.sift_down(freq, 1);
        top as usize
    }
}

/// Sets `len`, `code` and `max_code` from `freq`, and adds the block's
/// symbol bits to `cost`.
pub fn build(tree: &mut Tree, alphabet: &Alphabet, cost: &mut Cost) {
    let mut heap = Heap {
        slots: [0; HEAP_SIZE],
        len: 0,
        depth: [0; HEAP_SIZE],
    };
    let mut max_code = None;
    for n in 0..alphabet.symbols {
        if tree.freq[n] != 0 {
            heap.push_unordered(n);
            max_code = Some(n);
        } else {
            tree.len[n] = 0;
        }
    }
    // the format needs at least two codes in every tree
    while heap.len < 2 {
        let node = match max_code {
            None => 0,
            Some(n) if n < 2 => n + 1,
            Some(_) => 0,
        };
        if max_code.is_none_or(|n| n < 2) {
            max_code = Some(node);
        }
        heap.push_unordered(node);
        tree.freq[node] = 1;
        cost.dynamic = cost.dynamic.wrapping_sub(1);
        if let Some(fixed) = alphabet.fixed_len {
            cost.fixed = cost.fixed.wrapping_sub(u64::from(fixed[node]));
        }
    }
    let max_code = max_code.unwrap_or(0);
    tree.max_code = max_code;

    for n in (1..=heap.len / 2).rev() {
        heap.sift_down(&tree.freq, n);
    }
    let mut node = alphabet.symbols;
    let mut sorted = HEAP_SIZE;
    loop {
        let n = heap.pop(&tree.freq);
        let m = heap.slots[1] as usize;
        sorted -= 2;
        heap.slots[sorted + 1] = n as u16;
        heap.slots[sorted] = m as u16;

        tree.freq[node] = tree.freq[n] + tree.freq[m];
        heap.depth[node] = heap.depth[n].max(heap.depth[m]) + 1;
        tree.parent[n] = node as u16;
        tree.parent[m] = node as u16;
        heap.slots[1] = node as u16;
        node += 1;
        heap.sift_down(&tree.freq, 1);
        if heap.len < 2 {
            break;
        }
    }
    sorted -= 1;
    heap.slots[sorted] = heap.slots[1];

    assign_lengths(tree, alphabet, &heap.slots, sorted, cost);
    assign_codes(&tree.len[..=max_code], &mut tree.code[..=max_code]);
}

/// `order[first..]` holds every node, root first, each parent before its
/// children.
fn assign_lengths(
    tree: &mut Tree,
    alphabet: &Alphabet,
    order: &[u16; HEAP_SIZE],
    first: usize,
    cost: &mut Cost,
) {
    let max_len = alphabet.max_len;
    let mut count = [0u16; MAX_BITS + 1];
    let mut overflow = 0i32;

    tree.len[order[first] as usize] = 0;
    for &n in &order[first + 1..] {
        let n = n as usize;
        let mut bits = tree.len[tree.parent[n] as usize] + 1;
        if bits > max_len {
            bits = max_len;
            overflow += 1;
        }
        tree.len[n] = bits;
        if n > tree.max_code {
            continue;
        }
        count[bits as usize] += 1;
        let extra = if n >= alphabet.extra_base {
            alphabet.extra[n - alphabet.extra_base]
        } else {
            0
        };
        let freq = u64::from(tree.freq[n]);
        cost.dynamic = cost.dynamic.wrapping_add(freq * u64::from(bits + extra));
        if let Some(fixed) = alphabet.fixed_len {
            cost.fixed = cost.fixed.wrapping_add(freq * u64::from(fixed[n] + extra));
        }
    }
    if overflow == 0 {
        return;
    }

    // move leaves down until the too-long codes fit, the way zlib does:
    // this is not the optimal length-limited code
    let max = max_len as usize;
    while overflow > 0 {
        let mut bits = max - 1;
        while count[bits] == 0 {
            bits -= 1;
        }
        count[bits] -= 1;
        count[bits + 1] += 2;
        count[max] -= 1;
        overflow -= 2;
    }
    // hand the lengths out again, longest to the least frequent
    let mut h = HEAP_SIZE;
    for bits in (1..=max).rev() {
        let mut left = count[bits];
        while left != 0 {
            h -= 1;
            let m = order[h] as usize;
            if m > tree.max_code {
                continue;
            }
            let old = u64::from(tree.len[m]);
            if old != bits as u64 {
                let delta = (bits as u64).wrapping_sub(old);
                cost.dynamic = cost
                    .dynamic
                    .wrapping_add(delta.wrapping_mul(u64::from(tree.freq[m])));
                tree.len[m] = bits as u8;
            }
            left -= 1;
        }
    }
}
