//! hashing input that arrives in pieces

use super::tree::{self, Output};
use super::{Backend, CHUNK_END, CHUNK_LEN, CHUNK_START, DERIVE_KEY_CONTEXT};
use super::{DERIVE_KEY_MATERIAL, Hash, IV, KEY_LEN, KEYED_HASH, key_words};
use std::fmt;

/// a chunk whose bytes are still arriving
#[derive(Clone)]
struct Chunk {
    cv: [u32; 8],
    counter: u64,
    /// the last block, compressed only once more input shows that it is not the
    /// last; zero beyond `filled`
    block: [u8; 64],
    filled: usize,
    compressed: usize,
}

impl Chunk {
    fn new(key: &[u32; 8], counter: u64) -> Self {
        Self {
            cv: *key,
            counter,
            block: [0; 64],
            filled: 0,
            compressed: 0,
        }
    }

    fn len(&self) -> usize {
        self.compressed * 64 + self.filled
    }

    fn start_flag(&self) -> u32 {
        if self.compressed == 0 { CHUNK_START } else { 0 }
    }

    /// takes input that fits in this chunk
    fn update(&mut self, mut input: &[u8], flags: u32, backend: Backend) {
        if self.filled > 0 {
            let take = (64 - self.filled).min(input.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&input[..take]);
            self.filled += take;
            input = &input[take..];
            if input.is_empty() {
                return;
            }
            let block_flags = flags | self.start_flag();
            self.cv = backend.compress(&self.cv, &self.block, 64, self.counter, block_flags);
            self.compressed += 1;
            self.block = [0; 64];
            self.filled = 0;
        }
        let whole = input.len().saturating_sub(1) / 64;
        let (blocks, rest) = input.split_at(whole * 64);
        let (blocks, start) = (blocks.as_chunks().0, self.start_flag());
        backend.absorb(&mut self.cv, blocks, self.counter, flags, start);
        self.compressed += whole;
        input = rest;
        self.block[..input.len()].copy_from_slice(input);
        self.filled = input.len();
    }

    fn output(&self, flags: u32) -> Output {
        Output {
            cv: self.cv,
            block: self.block,
            block_len: self.filled as u32,
            counter: self.counter,
            flags: flags | self.start_flag() | CHUNK_END,
        }
    }
}

/// a tree as deep as an input of 2^64 bytes needs, plus the pair pushed together
const STACK: usize = 55;

/// streaming BLAKE3
#[derive(Clone)]
pub struct Hasher {
    key: [u32; 8],
    flags: u32,
    backend: Backend,
    chunk: Chunk,
    /// Chaining values of finished subtrees, left to right, each larger than the
    /// next. Two of equal size are only merged when more input follows, because
    /// the last merge of all has to be the root compression.
    stack: [[u8; 32]; STACK],
    stacked: usize,
}

impl Hasher {
    fn with(key: [u32; 8], flags: u32) -> Self {
        Self {
            key,
            flags,
            backend: Backend::detect(),
            chunk: Chunk::new(&key, 0),
            stack: [[0; 32]; STACK],
            stacked: 0,
        }
    }

    pub fn new() -> Self {
        Self::with(IV, 0)
    }

    /// the streaming form of `keyed_hash`
    pub fn new_keyed(key: &[u8; KEY_LEN]) -> Self {
        Self::with(key_words(key), KEYED_HASH)
    }

    /// the streaming form of `derive_key`, fed the key material
    pub fn new_derive_key(context: &str) -> Self {
        let backend = Backend::detect();
        let context_key = tree::root(context.as_bytes(), &IV, DERIVE_KEY_CONTEXT, backend);
        Self::with(key_words(context_key.as_bytes()), DERIVE_KEY_MATERIAL)
    }

    /// the same hasher running a chosen implementation
    pub fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = backend;
        self
    }

    /// forgets the input, keeps the key
    pub fn reset(&mut self) -> &mut Self {
        self.chunk = Chunk::new(&self.key, 0);
        self.stacked = 0;
        self
    }

    /// bytes hashed so far
    pub fn count(&self) -> u64 {
        self.chunk.counter * CHUNK_LEN as u64 + self.chunk.len() as u64
    }

    fn merged(&self, left: &[u8; 32], right: &[u8; 32]) -> Output {
        let mut children = [0; 64];
        children[..32].copy_from_slice(left);
        children[32..].copy_from_slice(right);
        tree::parent(&children, &self.key, self.flags)
    }

    /// Merges the subtrees that the input before chunk `chunks` completed: one
    /// stack entry remains per one bit of `chunks`.
    fn merge(&mut self, chunks: u64) {
        while self.stacked > chunks.count_ones() as usize {
            let merged = self.merged(&self.stack[self.stacked - 2], &self.stack[self.stacked - 1]);
            self.stack[self.stacked - 2] = merged.chaining_value(self.backend);
            self.stacked -= 1;
        }
    }

    /// adds the chaining value of a subtree that starts at chunk `chunks`
    fn push(&mut self, cv: [u8; 32], chunks: u64) {
        self.merge(chunks);
        self.stack[self.stacked] = cv;
        self.stacked += 1;
    }

    pub fn update(&mut self, mut input: &[u8]) -> &mut Self {
        let (key, flags, backend) = (self.key, self.flags, self.backend);
        if self.chunk.len() > 0 {
            let take = (CHUNK_LEN - self.chunk.len()).min(input.len());
            self.chunk.update(&input[..take], flags, backend);
            input = &input[take..];
            if input.is_empty() {
                return self;
            }
            let counter = self.chunk.counter;
            self.push(self.chunk.output(flags).chaining_value(backend), counter);
            self.chunk = Chunk::new(&key, counter + 1);
        }
        // Whole subtrees go to the wide functions directly. A subtree has to start at
        // a multiple of its own size. A single chunk that could still be the whole
        // input stays in `self.chunk`, where it can become the root.
        while input.len() > CHUNK_LEN {
            let counter = self.chunk.counter;
            let aligned = match counter {
                0 => u64::MAX,
                _ => (CHUNK_LEN as u64) << counter.trailing_zeros(),
            };
            let len = (CHUNK_LEN as u64) << (input.len() / CHUNK_LEN).ilog2();
            let len = len.min(aligned) as usize;
            let chunks = (len / CHUNK_LEN) as u64;
            if len == CHUNK_LEN {
                let whole = tree::chunk(&input[..len], &key, counter, flags, backend);
                self.push(whole.chaining_value(backend), counter);
            } else {
                let pair = tree::pair(&input[..len], &key, counter, flags, backend);
                let (left, right) = pair.split_at(32);
                self.push(left.try_into().unwrap(), counter);
                self.push(right.try_into().unwrap(), counter + chunks / 2);
            }
            self.chunk.counter += chunks;
            input = &input[len..];
        }
        if !input.is_empty() {
            self.chunk.update(input, flags, backend);
            // more input exists, so nothing merged here can be the root
            self.merge(self.chunk.counter);
        }
        self
    }

    pub fn finalize(&self) -> Hash {
        let backend = self.backend;
        if self.stacked == 0 {
            return self.chunk.output(self.flags).root(backend);
        }
        // The rightmost node so far is the chunk in progress or, when the input
        // ended on a subtree boundary, the parent of the two entries pushed last.
        // Every remaining entry joins it from the left, smallest subtree first.
        let (mut output, rest) = if self.chunk.len() > 0 {
            (self.chunk.output(self.flags), self.stacked)
        } else {
            let (left, right) = (&self.stack[self.stacked - 2], &self.stack[self.stacked - 1]);
            (self.merged(left, right), self.stacked - 2)
        };
        for left in self.stack[..rest].iter().rev() {
            output = self.merged(left, &output.chaining_value(backend));
        }
        output.root(backend)
    }
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Hasher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hasher")
            .field("count", &self.count())
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl std::io::Write for Hasher {
    #[inline]
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        self.update(input);
        Ok(input.len())
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
