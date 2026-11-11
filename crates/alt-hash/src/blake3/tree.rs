//! the tree over the chunks, hashed a whole batch of lanes at a time

use super::lanes::{Batch, Short, alike, cv_bytes};
use super::{Backend, CHUNK_END, CHUNK_LEN, CHUNK_START, Hash, MAX_LANES, PARENT, ROOT};

/// a chunk or parent node just before its last compression, which decides whether it
/// yields a chaining value or the root hash
pub(super) struct Output {
    pub cv: [u32; 8],
    pub block: [u8; 64],
    pub block_len: u32,
    pub counter: u64,
    pub flags: u32,
}

impl Output {
    #[inline]
    pub fn chaining_value(&self, backend: Backend) -> [u8; 32] {
        cv_bytes(&backend.compress(
            &self.cv,
            &self.block,
            self.block_len,
            self.counter,
            self.flags,
        ))
    }

    #[inline]
    pub fn root(&self, backend: Backend) -> Hash {
        let flags = self.flags | ROOT;
        Hash::from_bytes(cv_bytes(&backend.compress(
            &self.cv,
            &self.block,
            self.block_len,
            0,
            flags,
        )))
    }
}

/// one chunk of at most 1024 bytes, all of it at hand
#[inline]
pub(super) fn chunk(
    input: &[u8],
    key: &[u32; 8],
    counter: u64,
    flags: u32,
    backend: Backend,
) -> Output {
    // the last block is kept back even when it is full
    let whole = input.len().saturating_sub(1) / 64;
    let (blocks, rest) = input.split_at(whole * 64);
    let mut cv = *key;
    if whole != 0 {
        backend.absorb(&mut cv, blocks.as_chunks().0, counter, flags, CHUNK_START);
    }
    let mut block = [0; 64];
    // a fixed-size copy for the common full block instead of a call
    match rest.first_chunk::<64>() {
        Some(full) => block = *full,
        None => block[..rest.len()].copy_from_slice(rest),
    }
    let start = if whole == 0 { CHUNK_START } else { 0 };
    Output {
        cv,
        block,
        block_len: rest.len() as u32,
        counter,
        flags: flags | start | CHUNK_END,
    }
}

/// the parent of two chaining values that lie side by side
#[inline]
pub(super) fn parent(children: &[u8; 64], key: &[u32; 8], flags: u32) -> Output {
    Output {
        cv: *key,
        block: *children,
        block_len: 64,
        counter: 0,
        flags: flags | PARENT,
    }
}

/// Chaining values of the chunks in `input`, at most as many as the backend has
/// lanes; the last chunk may be short.
fn chunks(
    input: &[u8],
    key: &[u32; 8],
    counter: u64,
    flags: u32,
    backend: Backend,
    out: &mut [u8],
) -> usize {
    let whole = input.len() / CHUNK_LEN;
    let (full, rest) = input.split_at(whole * CHUNK_LEN);
    let batch = Batch {
        key,
        node_len: CHUNK_LEN,
        increment: true,
        flags,
        flags_start: CHUNK_START,
        flags_end: CHUNK_END,
    };
    // Chunks that do not fill a group of lanes, the last of them perhaps short, are
    // still compressed side by side when they are alike enough.
    let lanes = match backend.ragged_tail() {
        true => backend.ragged_lanes(),
        false => 0,
    };
    let grouped = if lanes == 0 {
        whole
    } else {
        whole - whole % lanes
    };
    let (first, tail) = input.split_at(grouped * CHUNK_LEN);
    let mut short = [Short {
        input: &[],
        counter: 0,
        root: false,
    }; MAX_LANES];
    let mut count = 0;
    for piece in tail.chunks(CHUNK_LEN) {
        short[count].input = piece;
        short[count].counter = counter + (grouped + count) as u64;
        count += 1;
    }
    if lanes != 0 && alike(&short[..count]) {
        backend.hash_many(&batch, first, counter, out);
        let tail_out = &mut out[grouped * 32..(grouped + count) * 32];
        backend.ragged(key, flags, &short[..count], tail_out);
        return grouped + count;
    }
    backend.hash_many(&batch, full, counter, out);
    if rest.is_empty() {
        return whole;
    }
    let last = chunk(rest, key, counter + whole as u64, flags, backend);
    out[whole * 32..whole * 32 + 32].copy_from_slice(&last.chaining_value(backend));
    whole + 1
}

/// Chaining values of the parents of each pair in `children`; an odd one out is
/// passed on as it is.
fn parents(children: &[u8], key: &[u32; 8], flags: u32, backend: Backend, out: &mut [u8]) -> usize {
    let pairs = children.len() / 64;
    let (paired, single) = children.split_at(pairs * 64);
    let batch = Batch {
        key,
        node_len: 64,
        increment: false,
        flags: flags | PARENT,
        flags_start: 0,
        flags_end: 0,
    };
    backend.hash_many(&batch, paired, 0, out);
    if single.is_empty() {
        return pairs;
    }
    out[pairs * 32..pairs * 32 + 32].copy_from_slice(single);
    pairs + 1
}

/// the bytes of the left subtree of an input longer than one chunk: the largest
/// power of two of whole chunks that leaves something for the right subtree
#[inline]
fn left_len(len: usize) -> usize {
    let chunks = (len - 1) / CHUNK_LEN;
    CHUNK_LEN << chunks.ilog2()
}

/// Reduces a subtree that starts at chunk `counter` to a row of chaining values
/// narrow enough for one batch (never the single root value) and returns how many
/// were written to `out`. Splitting at `left_len` keeps both halves made of whole
/// batches, so every level below is compressed at full width.
fn wide(
    input: &[u8],
    key: &[u32; 8],
    counter: u64,
    flags: u32,
    backend: Backend,
    out: &mut [u8],
) -> usize {
    let lanes = backend.lanes();
    if input.len() <= lanes * CHUNK_LEN {
        return chunks(input, key, counter, flags, backend, out);
    }
    let (left, right) = input.split_at(left_len(input.len()));
    let right_counter = counter + (left.len() / CHUNK_LEN) as u64;
    let mut children = [0; 2 * MAX_LANES * 32];
    // one lane would return a single value per side; two keep a pair for the caller
    let width = if lanes == 1 && left.len() > CHUNK_LEN {
        2
    } else {
        lanes
    };
    let (left_out, right_out) = children.split_at_mut(width * 32);
    let left_count = wide(left, key, counter, flags, backend, left_out);
    let right_count = wide(right, key, right_counter, flags, backend, right_out);
    // the left side is a full subtree, so it filled its half and the two are adjacent
    debug_assert_eq!(left_count, width);
    if left_count == 1 {
        out[..64].copy_from_slice(&children[..64]);
        return 2;
    }
    let count = left_count + right_count;
    parents(&children[..count * 32], key, flags, backend, out)
}

/// the two chaining values under the top node of a subtree longer than one chunk
pub(super) fn pair(
    input: &[u8],
    key: &[u32; 8],
    counter: u64,
    flags: u32,
    backend: Backend,
) -> [u8; 64] {
    let mut row = [0; MAX_LANES * 32];
    let mut count = wide(input, key, counter, flags, backend, &mut row);
    while count > 2 {
        let mut above = [0; MAX_LANES * 16 + 32];
        count = parents(&row[..count * 32], key, flags, backend, &mut above);
        row[..count * 32].copy_from_slice(&above[..count * 32]);
    }
    *row.first_chunk::<64>().unwrap()
}

/// the hash of a whole input
#[inline]
pub(super) fn root(input: &[u8], key: &[u32; 8], flags: u32, backend: Backend) -> Hash {
    if input.len() <= CHUNK_LEN {
        return chunk(input, key, 0, flags, backend).root(backend);
    }
    parent(&pair(input, key, 0, flags, backend), key, flags).root(backend)
}

/// Below this a subtree is not worth handing to another thread. Measured on sixteen
/// x86 threads: 16 KiB and 64 KiB are each slower somewhere between 128 KiB and 1 MiB.
const THREAD_LEN: usize = 32 << 10;

/// The hash of a whole input, with the upper levels of the tree spread over the
/// thread pool: each split offers the right half to another thread, `depth` times over.
pub(super) fn root_threads(
    input: &[u8],
    key: &[u32; 8],
    flags: u32,
    backend: Backend,
    depth: u32,
) -> Hash {
    if input.len() <= CHUNK_LEN {
        return chunk(input, key, 0, flags, backend).root(backend);
    }
    parent(
        &pair_threads(input, key, 0, flags, backend, depth),
        key,
        flags,
    )
    .root(backend)
}

/// `pair`, splitting across threads while `depth` and the length allow
fn pair_threads(
    input: &[u8],
    key: &[u32; 8],
    counter: u64,
    flags: u32,
    backend: Backend,
    depth: u32,
) -> [u8; 64] {
    // the right half is the shorter one, and may be as short as a byte
    let (left, right) = input.split_at(left_len(input.len()));
    if depth == 0 || right.len() < THREAD_LEN {
        return pair(input, key, counter, flags, backend);
    }
    let right_counter = counter + (left.len() / CHUNK_LEN) as u64;
    // both halves are longer than a chunk here, so each has a pair under its top node
    let value = |half: &[u8], counter: u64| {
        let children = pair_threads(half, key, counter, flags, backend, depth - 1);
        parent(&children, key, flags).chaining_value(backend)
    };
    let (left_value, right_value) =
        alt_threads::join(|| value(left, counter), || value(right, right_counter));
    let mut children = [0; 64];
    children[..32].copy_from_slice(&left_value);
    children[32..].copy_from_slice(&right_value);
    children
}
