//! What the match finders share: the view of prefix and data as one index
//! space, match measurement, and the store the sequences go to.

use std::hint::select_unpredictable;
use std::marker::PhantomData;

use crate::bits::{load32, load64};

/// Bytes a position must have ahead of it to be hashed or probed.
pub(crate) const PROBE: usize = 8;

/// Shortest match the finders emit.
pub(crate) const MIN_MATCH: usize = 4;

/// Positions without a match are skipped faster the longer the dry spell:
/// the step between probes grows by one for every `SKIP_SPAN` bytes passed.
pub(crate) const SKIP_SPAN: usize = 256;

/// Stand-ins for a candidate that may not be read. One of the two differs
/// from any data in its first byte, so comparing against it fails without
/// a branch on the candidate's validity.
pub(crate) static NOWHERE: [[u8; PROBE]; 2] = [[0xFF; PROBE], [0; PROBE]];

/// Multiplier that hashes the low `width` bytes of a word: a prime moved up
/// so that the product keeps nothing of the bytes above them.
pub(crate) const fn multiplier(width: u32) -> u64 {
    let prime: u64 = match width {
        4 => 2_654_435_761,
        5 => 889_523_592_379,
        6 => 227_718_039_650_203,
        7 => 58_295_818_150_454_627,
        _ => 0xCF1B_BCDC_B7A5_6463,
    };
    prime << (64 - 8 * width)
}

/// Hash of `value` by a [`multiplier`], `64 - shift` bits wide.
#[inline(always)]
pub(crate) fn hash(value: u64, multiplier: u64, shift: u32) -> usize {
    (value.wrapping_mul(multiplier) >> shift) as usize
}

/// One literal run followed by one match, in 8 bytes: the lengths keep
/// their low 16 bits. A block is too short for two lengths beyond that; the
/// one it may hold is named by [`SeqStore::long`].
#[derive(Clone, Copy)]
pub(crate) struct Sequence {
    /// 1..=3 names a repeat offset, anything larger is the distance plus 3
    pub offset: u32,
    pub lit_len: u16,
    /// match length less [`MATCH_BASE`]
    pub match_base: u16,
}

/// Shortest match the format can express.
pub(crate) const MATCH_BASE: usize = 3;

/// The length of a block that does not fit 16 bits, by sequence index.
#[derive(Clone, Copy)]
pub(crate) enum LongLength {
    Literals(usize),
    Match(usize),
}

/// Prefix and data seen as one run of bytes: the prefix occupies indices
/// `first..split`, the data `split..end`. Methods take `PREFIX`, which says
/// whether a prefix exists at all; without one they compile to plain
/// accesses of the data.
pub(crate) struct Window<'a> {
    /// prefix pointer biased so that adding an index below `split` lands in it
    prefix: *const u8,
    /// data pointer biased so that adding an index from `split` on lands in it
    data: *const u8,
    /// index of the first byte of history; never zero, so that an empty
    /// hash-table slot cannot look like a candidate
    pub first: usize,
    pub split: usize,
    pub end: usize,
    life: PhantomData<&'a [u8]>,
}

impl<'a> Window<'a> {
    pub(crate) fn new(prefix: &'a [u8], data: &'a [u8], first: usize) -> Self {
        let split = first + prefix.len();
        Self {
            prefix: prefix.as_ptr().wrapping_sub(first),
            data: data.as_ptr().wrapping_sub(split),
            first,
            split,
            end: split + data.len(),
            life: PhantomData,
        }
    }

    pub(crate) fn has_prefix(&self) -> bool {
        self.split != self.first
    }

    /// Address of the byte at `index`, which must lie in `first..end`.
    #[inline(always)]
    fn at<const PREFIX: bool>(&self, index: usize) -> *const u8 {
        if PREFIX && index < self.split {
            self.prefix.wrapping_add(index)
        } else {
            self.data.wrapping_add(index)
        }
    }

    /// The byte at `index`.
    ///
    /// # Safety
    /// `index` must lie in `first..end`.
    #[inline(always)]
    pub(crate) unsafe fn byte<const PREFIX: bool>(&self, index: usize) -> u8 {
        // safety: the caller keeps the index inside the window
        unsafe { self.at::<PREFIX>(index).read() }
    }

    /// Address of the data byte at `index`, which must lie in `split..end`.
    #[inline(always)]
    pub(crate) fn data_at(&self, index: usize) -> *const u8 {
        self.data.wrapping_add(index)
    }

    /// # Safety
    /// `index >= split` and `index + 8 <= end`.
    #[inline(always)]
    pub(crate) unsafe fn data64(&self, index: usize) -> u64 {
        // safety: the caller keeps the read inside the data
        unsafe { load64(self.data_at(index)) }
    }

    /// The 8 bytes of the prefix at `index`.
    ///
    /// # Safety
    /// `first <= index` and `index + 8 <= split`.
    pub(crate) unsafe fn prefix64(&self, index: usize) -> u64 {
        // safety: the caller keeps the read inside the prefix
        unsafe { load64(self.prefix.wrapping_add(index)) }
    }

    /// Whether `index` may be read as a candidate: at or above `low`, and
    /// not straddling the end of the prefix.
    #[inline(always)]
    fn usable<const PREFIX: bool>(&self, index: usize, low: usize) -> bool {
        index >= low && (!PREFIX || index >= self.split || index + PROBE <= self.split)
    }

    /// Whether the candidate at `index` is usable and starts with the four
    /// bytes `here`.
    ///
    /// Where the compiler would turn the usability test into a branch of
    /// its own, which mispredicts at the start of a frame, an unusable
    /// candidate is instead read from a stand-in that differs from `here`
    /// in its first byte.
    ///
    /// # Safety
    /// `index` must be below a data position that has 8 bytes ahead of it.
    #[inline(always)]
    pub(crate) unsafe fn hits32<const PREFIX: bool>(
        &self,
        index: usize,
        low: usize,
        here: u32,
    ) -> bool {
        let usable = self.usable::<PREFIX>(index, low);
        // safety: a usable candidate has 8 readable bytes, as does a stand-in
        unsafe {
            if cfg!(target_arch = "x86_64") {
                let nowhere = NOWHERE[(here as u8 == 0xFF) as usize].as_ptr();
                let from = select_unpredictable(usable, self.at::<PREFIX>(index), nowhere);
                load32(from) == here
            } else {
                let from = if usable {
                    self.at::<PREFIX>(index)
                } else {
                    NOWHERE[0].as_ptr()
                };
                (load32(from) == here) & usable
            }
        }
    }

    /// As [`Window::hits32`], written so that no architecture turns the
    /// usability test into a branch: on a short input, where the tables
    /// start out empty, that branch would be mispredicted often.
    ///
    /// # Safety
    /// `index` must be below a data position that has 8 bytes ahead of it.
    #[inline(always)]
    pub(crate) unsafe fn hits32_unlikely<const PREFIX: bool>(
        &self,
        index: usize,
        low: usize,
        here: u32,
    ) -> bool {
        let usable = self.usable::<PREFIX>(index, low);
        // safety: a usable candidate has 8 readable bytes, as does a stand-in
        unsafe {
            let from = select_unpredictable(usable, self.at::<PREFIX>(index), NOWHERE[0].as_ptr());
            ((load32(from) ^ here) | !usable as u32) == 0
        }
    }

    /// As [`Window::hits32`], with the usability test as a plain branch.
    /// On a long input unusable candidates are rare, the branch predicts
    /// well, and the candidate can be read without waiting for the test.
    ///
    /// # Safety
    /// `index` must be below a data position that has 8 bytes ahead of it.
    #[inline(always)]
    pub(crate) unsafe fn hits32_likely<const PREFIX: bool>(
        &self,
        index: usize,
        low: usize,
        here: u32,
    ) -> bool {
        // safety: a usable candidate has 8 readable bytes
        self.usable::<PREFIX>(index, low) && unsafe { load32(self.at::<PREFIX>(index)) } == here
    }

    /// As [`Window::hits32`], for the eight bytes `here`.
    ///
    /// # Safety
    /// `index` must be below a data position that has 8 bytes ahead of it.
    #[inline(always)]
    pub(crate) unsafe fn hits64<const PREFIX: bool>(
        &self,
        index: usize,
        low: usize,
        here: u64,
    ) -> bool {
        let usable = self.usable::<PREFIX>(index, low);
        // safety: a usable candidate has 8 readable bytes, as does a stand-in
        unsafe {
            if cfg!(target_arch = "x86_64") {
                let nowhere = NOWHERE[(here as u8 == 0xFF) as usize].as_ptr();
                let from = select_unpredictable(usable, self.at::<PREFIX>(index), nowhere);
                load64(from) == here
            } else {
                let from = if usable {
                    self.at::<PREFIX>(index)
                } else {
                    NOWHERE[0].as_ptr()
                };
                (load64(from) == here) & usable
            }
        }
    }

    /// Whether the 4 bytes at `ip` repeat those `rep` bytes earlier; a `rep`
    /// of zero stands for no offset.
    ///
    /// # Safety
    /// `ip` in the data with 4 bytes ahead of it, `ip - rep >= first`.
    #[inline(always)]
    pub(crate) unsafe fn repeats<const PREFIX: bool>(&self, ip: usize, rep: usize) -> bool {
        // safety: with rep == 0 both sides read the same data bytes; a
        // source in the prefix is read only if 8 bytes of it remain there
        unsafe {
            let here = load32(self.data_at(ip));
            (rep != 0) & self.hits32::<PREFIX>(ip - rep, 0, here)
        }
    }

    /// Length of the common run of the bytes at `ip` (in the data) and at
    /// the earlier index `from`, stopping at `end`.
    ///
    /// # Safety
    /// `first <= from < ip`, `split <= ip <= end <= self.end`.
    #[inline(always)]
    pub(crate) unsafe fn common<const PREFIX: bool>(
        &self,
        ip: usize,
        from: usize,
        end: usize,
    ) -> usize {
        // safety: every comparison stays below `end` on the ip side, and the
        // other side trails it within the same index space
        unsafe {
            if !PREFIX || from >= self.split {
                return run(self.data_at(ip), self.data_at(from), end - ip);
            }
            let in_prefix = (self.split - from).min(end - ip);
            let len = run(self.data_at(ip), self.prefix.wrapping_add(from), in_prefix);
            if from + len < self.split {
                return len;
            }
            // the earlier side ran off the prefix: it continues in the data
            len + run(
                self.data_at(ip + len),
                self.data_at(self.split),
                end - ip - len,
            )
        }
    }

    /// Grows a match backwards: steps `ip` and `from` down while the bytes
    /// before them agree, without passing `anchor` or `low`. Returns the
    /// number of steps.
    ///
    /// # Safety
    /// `low >= first`, `anchor >= split`, and both indices inside the window.
    #[inline(always)]
    pub(crate) unsafe fn extend_back<const PREFIX: bool>(
        &self,
        ip: &mut usize,
        from: &mut usize,
        anchor: usize,
        low: usize,
    ) -> usize {
        let start = *ip;
        // safety: both indices stay at or above `first` and below `end`
        unsafe {
            while *ip > anchor
                && *from > low
                && self.data_at(*ip - 1).read() == self.at::<PREFIX>(*from - 1).read()
            {
                *ip -= 1;
                *from -= 1;
            }
        }
        start - *ip
    }
}

/// Number of equal leading bytes of two ranges of `max` bytes.
///
/// # Safety
/// Both pointers must be valid for `max` bytes of reads.
#[inline(always)]
unsafe fn run(a: *const u8, b: *const u8, max: usize) -> usize {
    let mut len = 0;
    // safety: every read ends at or before `max`
    unsafe {
        while len + 8 <= max {
            let diff = load64(a.add(len)) ^ load64(b.add(len));
            if diff != 0 {
                return len + (diff.trailing_zeros() >> 3) as usize;
            }
            len += 8;
        }
        while len < max && a.add(len).read() == b.add(len).read() {
            len += 1;
        }
    }
    len
}

/// Copies `len` bytes 16 at a time, up to 15 too many. Long literal runs
/// are rare: a call to this costs less than one to a copy that must work
/// for every length, and the loops that store sequences stay free of it.
///
/// # Safety
/// Both ranges must be valid for `len` rounded up to a multiple of 16.
#[inline(never)]
unsafe fn copy_wide(dst: *mut u8, src: *const u8, len: usize) {
    let mut done = 0;
    while done < len {
        // safety: the caller provides the rounded-up length on both sides
        unsafe {
            dst.add(done).copy_from_nonoverlapping(src.add(done), 16);
        }
        done += 16;
    }
}

/// The sequences and literals of one block.
pub(crate) struct SeqStore {
    pub sequences: Vec<Sequence>,
    pub literals: Vec<u8>,
    pub long: Option<LongLength>,
}

impl SeqStore {
    pub(crate) const fn new() -> Self {
        Self {
            sequences: Vec::new(),
            literals: Vec::new(),
            long: None,
        }
    }

    /// Empties the store and makes room for a block of `block_len` bytes.
    pub(crate) fn start(&mut self, block_len: usize) {
        self.sequences.clear();
        self.sequences.reserve(block_len / MIN_MATCH + 1);
        self.literals.clear();
        self.literals.reserve(block_len + 16);
        self.long = None;
    }

    /// Records the literals `anchor..ip` and the match that follows them.
    ///
    /// # Safety
    /// `anchor..ip` must lie in the data of `window`; within one block,
    /// literals and matches (each at least `MIN_MATCH` long) together may
    /// not exceed the length given to [`SeqStore::start`], which may not
    /// exceed the largest block of the format.
    #[inline(always)]
    pub(crate) unsafe fn push(
        &mut self,
        window: &Window<'_>,
        anchor: usize,
        ip: usize,
        match_len: usize,
        offset: usize,
    ) {
        let lit_len = ip - anchor;
        let len = self.literals.len();
        // safety: the literal buffer has room for the block plus 16 bytes,
        // so a copy that rounds a run up to 16 bytes fits even at the end
        // of the block, which a match of at least MIN_MATCH bytes follows;
        // the source is read 16 wide only when that stays inside the data
        // (below `ip + 16` for the later steps of a long run); the sequence
        // buffer has room for one sequence per MIN_MATCH bytes of block
        unsafe {
            let dst = self.literals.as_mut_ptr().add(len);
            let src = window.data_at(anchor);
            if anchor + 16 <= window.end {
                dst.copy_from_nonoverlapping(src, 16);
                if lit_len > 16 {
                    if ip + 16 <= window.end {
                        copy_wide(dst.add(16), src.add(16), lit_len - 16);
                    } else {
                        dst.add(16)
                            .copy_from_nonoverlapping(src.add(16), lit_len - 16);
                    }
                }
            } else {
                dst.copy_from_nonoverlapping(src, lit_len);
            }
            self.literals.set_len(len + lit_len);
            let count = self.sequences.len();
            let match_base = match_len - MATCH_BASE;
            if (lit_len | match_base) > 0xFFFF {
                self.long = Some(if lit_len > 0xFFFF {
                    LongLength::Literals(count)
                } else {
                    LongLength::Match(count)
                });
            }
            self.sequences.as_mut_ptr().add(count).write(Sequence {
                offset: offset as u32,
                lit_len: lit_len as u16,
                match_base: match_base as u16,
            });
            self.sequences.set_len(count + 1);
        }
    }
}
