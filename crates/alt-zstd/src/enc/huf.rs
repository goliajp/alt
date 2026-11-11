//! Huffman encoding of literals: code construction, the tree description
//! and the bitstreams.

use super::fse::{CTable, normalize, table_log, write_distribution};
use crate::bits::BitWriter;
use crate::huffman::MAX_BITS;

/// A complete prefix code over bytes.
#[derive(Clone)]
pub(crate) struct Codes {
    /// code in the low 16 bits, its length above; zero for absent symbols
    pub packed: [u32; 256],
    /// length of the longest code
    pub log: u32,
}

impl Codes {
    pub(crate) fn new() -> Self {
        Self {
            packed: [0; 256],
            log: 0,
        }
    }

    #[inline(always)]
    pub(crate) fn len(&self, symbol: usize) -> u32 {
        self.packed[symbol] >> 16
    }

    /// Bits needed to code a histogram, or `None` if it has a symbol this
    /// code lacks.
    pub(crate) fn cost(&self, counts: &[u32; 256]) -> Option<u32> {
        let mut bits = 0;
        for (symbol, &count) in counts.iter().enumerate() {
            if count != 0 {
                if self.packed[symbol] == 0 {
                    return None;
                }
                bits += count * self.len(symbol);
            }
        }
        Some(bits)
    }
}

/// Builds the code for a histogram with at least two symbols present, no
/// code longer than `limit` bits.
pub(crate) fn build(counts: &[u32; 256], limit: u32) -> Codes {
    // leaves in ascending order of count
    let mut order = [0u32; 256];
    let mut leaves = 0;
    for (symbol, &count) in counts.iter().enumerate() {
        if count != 0 {
            order[leaves] = (count << 8) | symbol as u32;
            leaves += 1;
        }
    }
    let order = &mut order[..leaves];
    order.sort_unstable();

    // with sorted leaves the internal nodes come out sorted too, so the two
    // lightest nodes are always at the head of one of two queues
    let mut weight = [0u32; 512];
    let mut parent = [0u16; 512];
    for (node, &leaf) in order.iter().enumerate() {
        weight[node] = leaf >> 8;
    }
    let (mut leaf, mut inner) = (0, leaves);
    for node in leaves..2 * leaves - 1 {
        let mut lightest = || {
            if leaf < leaves && (inner >= node || weight[leaf] <= weight[inner]) {
                leaf += 1;
                leaf - 1
            } else {
                inner += 1;
                inner - 1
            }
        };
        let (a, b) = (lightest(), lightest());
        weight[node] = weight[a] + weight[b];
        parent[a] = node as u16;
        parent[b] = node as u16;
    }
    let mut depth = [0u8; 512];
    for node in (0..2 * leaves - 2).rev() {
        depth[node] = depth[parent[node] as usize] + 1;
    }

    // cap the lengths, then restore a complete code: each step trades one
    // code of the longest length and one shorter code for two codes one bit
    // longer than the latter
    let limit = limit as usize;
    let mut per_len = [0u32; MAX_BITS as usize + 1];
    for &d in &depth[..leaves] {
        per_len[(d as usize).min(limit)] += 1;
    }
    let mut filled: u32 = (1..=limit).map(|len| per_len[len] << (limit - len)).sum();
    while filled > 1 << limit {
        per_len[limit] -= 1;
        let len = (1..limit).rev().find(|&len| per_len[len] != 0).unwrap();
        per_len[len] -= 1;
        per_len[len + 1] += 2;
        filled -= 1;
    }
    let log = (1..=limit).rev().find(|&len| per_len[len] != 0).unwrap() as u32;

    // rarest symbols take the longest codes
    let mut lens = [0u8; 256];
    let mut next = 0;
    for len in (1..=limit).rev() {
        for _ in 0..per_len[len] {
            lens[(order[next] & 255) as usize] = len as u8;
            next += 1;
        }
    }

    // values follow the decoder's table: longest codes first, and within a
    // length in symbol order
    let mut codes = Codes::new();
    codes.log = log;
    let mut at = [0u32; MAX_BITS as usize + 2];
    let mut start = 0;
    for len in (1..=log).rev() {
        at[len as usize] = start;
        start += per_len[len as usize] << (log - len);
    }
    for (symbol, &len) in lens.iter().enumerate() {
        if len != 0 {
            let len = len as u32;
            codes.packed[symbol] = (len << 16) | (at[len as usize] >> (log - len));
            at[len as usize] += 1 << (log - len);
        }
    }
    codes
}

/// Appends the tree description for symbols `0..=max_symbol`. Returns false
/// (leaving `out` as it was) when the format cannot express it.
pub(crate) fn write_tree(codes: &Codes, max_symbol: usize, out: &mut Vec<u8>) -> bool {
    // the last symbol's weight is implied
    let count = max_symbol;
    let mut weights = [0u8; 256];
    for (symbol, weight) in weights[..count].iter_mut().enumerate() {
        if codes.packed[symbol] != 0 {
            *weight = (codes.log + 1 - codes.len(symbol)) as u8;
        }
    }
    let weights = &weights[..count];
    let at = out.len();
    out.push(0);
    if compress_weights(weights, out) {
        let size = out.len() - at - 1;
        if size > 1 && size < count / 2 {
            out[at] = size as u8;
            return true;
        }
    }
    out.truncate(at);
    if count > 128 {
        return false;
    }
    out.push(127 + count as u8);
    for pair in weights.chunks(2) {
        out.push(pair[0] << 4 | pair.get(1).copied().unwrap_or(0));
    }
    true
}

/// Appends the weights coded with two interleaved FSE states. Returns false
/// when they do not lend themselves to it.
fn compress_weights(weights: &[u8], out: &mut Vec<u8>) -> bool {
    const MAX_LOG: u32 = 6;
    let mut counts = [0u32; MAX_BITS as usize + 1];
    for &weight in weights {
        counts[weight as usize] += 1;
    }
    let total = weights.len() as u32;
    let most = *counts.iter().max().unwrap();
    if total < 3 || most == total || most == 1 {
        return false;
    }
    let symbols = counts.iter().rposition(|&count| count != 0).unwrap() + 1;
    let log = table_log(MAX_LOG, total, symbols - 1);
    let mut norm = [0i16; MAX_BITS as usize + 1];
    normalize(&counts[..symbols], total, log, &mut norm[..symbols]);
    write_distribution(&norm[..symbols], log, out);
    let mut table = CTable::new();
    table.build(&norm[..symbols], log);

    // even positions go through the first state, odd ones through the second
    let mut writer = BitWriter::new(out);
    let mut left = weights.len();
    let mut take = || {
        left -= 1;
        weights[left] as usize
    };
    let (mut first, mut second);
    if weights.len() & 1 == 1 {
        first = table.start(take());
        second = table.start(take());
        let (bits, width) = table.encode(&mut first, take());
        writer.put(bits, width);
    } else {
        second = table.start(take());
        first = table.start(take());
    }
    for _ in 0..(weights.len() - 2) / 2 {
        for state in [&mut second, &mut first] {
            let (bits, width) = table.encode(state, take());
            writer.put(bits, width);
        }
        writer.flush();
    }
    for state in [second, first] {
        let (bits, width) = table.last(state);
        writer.put(bits, width);
    }
    writer.finish_marked();
    true
}

/// Appends `src` as one bitstream.
pub(crate) fn encode_stream(codes: &Codes, src: &[u8], out: &mut Vec<u8>) {
    out.reserve(src.len() * MAX_BITS as usize / 8 + 16);
    let mut writer = BitWriter::new(out);
    let put = |writer: &mut BitWriter<'_>, byte: u8| {
        let packed = codes.packed[byte as usize];
        writer.put((packed & 0xFFFF) as u64, packed >> 16);
    };
    // the reader starts at the end of the stream, so the last byte goes first
    let (groups, odd) = src.as_chunks::<4>();
    for &byte in odd.iter().rev() {
        put(&mut writer, byte);
    }
    writer.flush();
    for group in groups.iter().rev() {
        put(&mut writer, group[3]);
        put(&mut writer, group[2]);
        put(&mut writer, group[1]);
        put(&mut writer, group[0]);
        writer.flush();
    }
    writer.finish_marked();
}

/// Appends `src` as four bitstreams behind their jump table. The four are
/// written side by side into `scratch`, so that none waits for another.
pub(crate) fn encode_4x(codes: &Codes, src: &[u8], scratch: &mut Vec<u8>, out: &mut Vec<u8>) {
    #[cfg(target_arch = "x86_64")]
    if crate::has_bmi2() {
        // safety: the processor was just found to have the features
        return unsafe { encode_4x_bmi2(codes, src, scratch, out) };
    }
    encode_4x_any(codes, src, scratch, out)
}

/// The same compiled for processors with BMI2, where the variable shifts
/// of bit writing need no fixed register.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "bmi1,bmi2,lzcnt")]
unsafe fn encode_4x_bmi2(codes: &Codes, src: &[u8], scratch: &mut Vec<u8>, out: &mut Vec<u8>) {
    encode_4x_any(codes, src, scratch, out)
}

#[inline(always)]
fn encode_4x_any(codes: &Codes, src: &[u8], scratch: &mut Vec<u8>, out: &mut Vec<u8>) {
    const GROUP: usize = 4;
    let segment = src.len().div_ceil(4);
    let parts: [&[u8]; 4] = std::array::from_fn(|i| {
        &src[(i * segment).min(src.len())..((i + 1) * segment).min(src.len())]
    });
    // room per stream: its longest possible encoding, the marker, and the
    // 8 bytes a flush stores
    let room = segment * MAX_BITS as usize / 8 + 16;
    scratch.clear();
    scratch.reserve(4 * room);
    let base = scratch.as_mut_ptr();
    let mut ends = [base; 4];

    // safety (whole block): stream i writes inside scratch[i * room..][..room]:
    // each flush stores 8 bytes at a position that is at most the stream's
    // encoded size so far, which stays below room - 8
    unsafe {
        let mut at: [*mut u8; 4] = std::array::from_fn(|i| base.add(i * room));
        let mut acc = [0u64; 4];
        let mut bits = [0u32; 4];
        let mut left = parts.map(<[u8]>::len);
        let put = |acc: &mut u64, bits: &mut u32, byte: u8| {
            let packed = *codes.packed.get_unchecked(byte as usize);
            *acc |= ((packed & 0xFFFF) as u64) << *bits;
            *bits += packed >> 16;
        };
        let flush = |at: &mut *mut u8, acc: &mut u64, bits: &mut u32| {
            at.cast::<u64>().write_unaligned(acc.to_le());
            *at = at.add((*bits >> 3) as usize);
            *acc >>= *bits & !7;
            *bits &= 7;
        };
        // the reader starts at the end of each stream, so last bytes go
        // first; bring all four to the same multiple of GROUP
        let groups = left[3] / GROUP;
        for i in 0..4 {
            while left[i] > groups * GROUP {
                left[i] -= 1;
                put(&mut acc[i], &mut bits[i], parts[i][left[i]]);
                flush(&mut at[i], &mut acc[i], &mut bits[i]);
            }
        }
        for group in (0..groups).rev() {
            for i in 0..4 {
                let bytes = parts[i].as_ptr().add(group * GROUP);
                for k in (0..GROUP).rev() {
                    put(&mut acc[i], &mut bits[i], bytes.add(k).read());
                }
                flush(&mut at[i], &mut acc[i], &mut bits[i]);
            }
        }
        for i in 0..4 {
            acc[i] |= 1 << bits[i];
            bits[i] += 1;
            flush(&mut at[i], &mut acc[i], &mut bits[i]);
            ends[i] = at[i].add((bits[i] != 0) as usize);
        }
    }

    // safety: each end lies in its stream's part of the scratch area, and
    // the bytes from the start of that part to the end were all written
    let streams: [&[u8]; 4] = std::array::from_fn(|i| unsafe {
        let start = base.add(i * room);
        std::slice::from_raw_parts(start, ends[i].offset_from(start) as usize)
    });
    for stream in &streams[..3] {
        out.extend_from_slice(&(stream.len() as u16).to_le_bytes());
    }
    for stream in streams {
        out.extend_from_slice(stream);
    }
}
