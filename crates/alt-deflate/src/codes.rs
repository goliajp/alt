//! Huffman code lengths for the general compressor.
//!
//! The code is as good as any other with the same length limit in the common case;
//! where the limit binds, lengths are adjusted the way zlib does, not optimally. It is
//! not the code zlib would choose, and it is several times cheaper to compute, which
//! is most of the work of compressing a small input.

use crate::{
    tables::{L_CODES, MAX_BITS, assign_codes},
    trees::{Alphabet, Cost, Tree},
};

/// sets `len`, `code` and `max_code` from `freq`, and adds the block's symbol bits to `cost`
pub fn build(tree: &mut Tree, alphabet: &Alphabet, cost: &mut Cost) {
    // used symbols as (frequency, symbol), so that sorting orders by frequency
    let mut order = [0u32; L_CODES];
    let mut used = 0;
    for symbol in 0..alphabet.symbols {
        tree.len[symbol] = 0;
        if tree.freq[symbol] != 0 {
            order[used] = u32::from(tree.freq[symbol]) << 16 | symbol as u32;
            used += 1;
        }
    }
    // the format wants two codes in every tree
    let mut spare = 0;
    while used < 2 {
        if tree.freq[spare] == 0 {
            tree.freq[spare] = 1;
            order[used] = 1 << 16 | spare as u32;
            used += 1;
        }
        spare += 1;
    }
    let order = &mut order[..used];
    order.sort_unstable();

    let mut depth = [0u32; L_CODES];
    for (slot, entry) in depth.iter_mut().zip(order.iter()) {
        *slot = entry >> 16;
    }
    depths(&mut depth[..used]);

    // codes per length, with the longer ones cut down to the limit
    let limit = alphabet.max_len as usize;
    let mut count = [0u32; MAX_BITS + 1];
    for bits in &depth[..used] {
        count[(*bits as usize).min(limit)] += 1;
    }
    // Cutting down takes more code space than there is, counted here in codes of the
    // limit's length. Each step gives one of them back: a code just below the limit
    // becomes a prefix of two codes, itself and one of those that were cut.
    let space = |count: &[u32; MAX_BITS + 1]| -> u32 {
        (1..=limit).map(|bits| count[bits] << (limit - bits)).sum()
    };
    for _ in 0..space(&count) - (1 << limit) {
        let bits = (1..limit).rev().find(|bits| count[*bits] != 0).unwrap();
        count[bits] -= 1;
        count[bits + 1] += 2;
        count[limit] -= 1;
    }
    assert_eq!(space(&count), 1 << limit);

    // the longest lengths go to the least frequent symbols
    let mut next = order.iter();
    let mut max_code = 0;
    for bits in (1..=limit).rev() {
        for entry in next.by_ref().take(count[bits] as usize) {
            let symbol = (entry & 0xffff) as usize;
            tree.len[symbol] = bits as u8;
            max_code = max_code.max(symbol);
            let extra = match symbol.checked_sub(alphabet.extra_base) {
                Some(index) => alphabet.extra[index],
                None => 0,
            };
            let frequency = u64::from(entry >> 16);
            cost.dynamic += frequency * (bits as u64 + u64::from(extra));
            if let Some(fixed) = alphabet.fixed_len {
                cost.fixed += frequency * u64::from(fixed[symbol] + extra);
            }
        }
    }
    tree.max_code = max_code;
    assign_codes(&tree.len[..=max_code], &mut tree.code[..=max_code]);
}

/// Replaces at least two frequencies, sorted ascending, by the depths of their leaves
/// in a Huffman tree, in place: first the weights of the internal nodes are formed
/// from two queues (leaves, and internal nodes already made), then their depths are
/// counted from the root, then the leaves are handed the depths left over.
fn depths(a: &mut [u32]) {
    let n = a.len();
    a[0] += a[1];
    let (mut root, mut leaf) = (0, 2);
    for next in 1..n - 1 {
        // the smaller of the next internal node and the next leaf, twice
        if leaf >= n || a[root] < a[leaf] {
            a[next] = a[root];
            a[root] = next as u32;
            root += 1;
        } else {
            a[next] = a[leaf];
            leaf += 1;
        }
        if leaf >= n || (root < next && a[root] < a[leaf]) {
            a[next] += a[root];
            a[root] = next as u32;
            root += 1;
        } else {
            a[next] += a[leaf];
            leaf += 1;
        }
    }
    // internal node `i` now holds the index of its parent; turn that into depth
    a[n - 2] = 0;
    for next in (0..n - 2).rev() {
        a[next] = a[a[next] as usize] + 1;
    }
    // leaves fill each depth's places that internal nodes leave free
    let (mut available, mut internal, mut depth) = (1usize, 0usize, 0u32);
    let mut root = n as isize - 2;
    let mut next = n as isize - 1;
    while available > 0 {
        while root >= 0 && a[root as usize] == depth {
            internal += 1;
            root -= 1;
        }
        while available > internal {
            a[next as usize] = depth;
            next -= 1;
            available -= 1;
        }
        available = 2 * internal;
        depth += 1;
        internal = 0;
    }
}
