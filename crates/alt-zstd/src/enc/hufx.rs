//! The four literal streams for x86-64 processors with BMI2. The streams
//! are written one after another, each through an accumulator that is
//! filled from the top: a code costs one table load, a shift, an or and an
//! add, and the processor's pipeline width is what bounds the loop.

use super::huf::Codes;
use crate::huffman::MAX_BITS;

/// A code as the accumulator takes it: its length in the low byte, its bits
/// at the top of the word.
fn entries(codes: &Codes) -> [u64; 256] {
    std::array::from_fn(|symbol| {
        let packed = codes.packed[symbol];
        let len = (packed >> 16) as u64;
        match len {
            0 => 0,
            _ => len | ((packed & 0xFFFF) as u64) << (64 - len),
        }
    })
}

/// Bits waiting to be stored, at the top of `acc` with the oldest lowest.
/// Only the low byte of `pending` counts. Below the waiting bits `acc`
/// holds leftovers that are never stored.
#[derive(Clone, Copy)]
struct Top {
    acc: u64,
    pending: u64,
}

impl Top {
    const EMPTY: Self = Self { acc: 0, pending: 0 };

    /// Adds a code. The entry's length byte comes along at the bottom of
    /// the word, so its set bits must stay below the waiting ones: the
    /// caller keeps `pending + 4 <= 64` after the call, or `+ 3` for
    /// lengths below 8.
    #[inline(always)]
    fn push(&mut self, entry: u64) {
        self.acc = self.acc.wrapping_shr(entry as u32) | entry;
        self.pending = self.pending.wrapping_add(entry);
    }

    /// Adds a code and nothing else; up to 64 bits may wait afterwards.
    #[inline(always)]
    fn push_clean(&mut self, entry: u64) {
        self.acc = self.acc.wrapping_shr(entry as u32) | (entry & !0xFF);
        self.pending = self.pending.wrapping_add(entry);
    }

    /// Appends the bits of `other`, which started out empty.
    #[inline(always)]
    fn append(&mut self, other: Self) {
        self.acc = self.acc.wrapping_shr(other.pending as u32) | other.acc;
        self.pending = self.pending.wrapping_add(other.pending);
    }

    /// Stores the waiting bits and advances `at` over the whole bytes among
    /// them; fewer than 8 keep waiting.
    ///
    /// # Safety
    /// `at` must be valid for 8 bytes of writes.
    #[inline(always)]
    unsafe fn flush(&mut self, at: &mut *mut u8) {
        let waiting = self.pending & 0xFF;
        // with nothing waiting the shift is by 0 and stores leftovers that
        // the next store overwrites
        let word = self.acc.wrapping_shr(64u32.wrapping_sub(waiting as u32));
        // safety: the caller provides the 8 bytes
        unsafe {
            at.cast::<u64>().write_unaligned(word.to_le());
            *at = at.add((waiting >> 3) as usize);
        }
        self.pending = waiting & 7;
    }
}

/// Writes `src` as one stream at `at` and returns its end. Codes go in
/// groups of `GROUP` between stores; `CLEAN` tells whether the last code of
/// a group could push the length byte of its entry into the waiting bits.
/// Every second group starts in an accumulator of its own, so that its
/// codes need not wait for the group before.
///
/// # Safety
/// `at` must be valid for writes of the encoded stream plus 8 bytes, every
/// byte of `src` must have a code, and `7 + GROUP * longest code <= 64`.
#[inline(always)]
unsafe fn stream<const GROUP: usize, const CLEAN: bool>(
    table: &[u64; 256],
    src: &[u8],
    mut at: *mut u8,
) -> *mut u8 {
    // safety (whole block): indices stay below `src.len()`, and each flush
    // stores at most the bytes encoded so far plus 8
    unsafe {
        let entry = |i: usize| *table.get_unchecked(*src.get_unchecked(i) as usize);
        let group = |top: &mut Top, end: usize| {
            for back in 1..GROUP {
                top.push(entry(end - back));
            }
            if CLEAN {
                top.push_clean(entry(end - GROUP));
            } else {
                top.push(entry(end - GROUP));
            }
        };
        // the reader starts at the end of the stream, so the last byte
        // goes first
        let mut top = Top::EMPTY;
        let mut left = src.len();
        if !left.is_multiple_of(GROUP) {
            while !left.is_multiple_of(GROUP) {
                left -= 1;
                top.push_clean(entry(left));
            }
            top.flush(&mut at);
        }
        if !left.is_multiple_of(2 * GROUP) {
            group(&mut top, left);
            top.flush(&mut at);
            left -= GROUP;
        }
        while left != 0 {
            group(&mut top, left);
            top.flush(&mut at);
            let mut second = Top::EMPTY;
            group(&mut second, left - GROUP);
            top.append(second);
            top.flush(&mut at);
            left -= 2 * GROUP;
        }
        // the marker bit that tells the reader where the stream ends
        top.push_clean(1 | 1 << 63);
        top.flush(&mut at);
        at.add((top.pending != 0) as usize)
    }
}

/// Appends `src` as four bitstreams behind their jump table.
///
/// # Safety
/// The processor must have BMI2, and every byte of `src` a code in `codes`.
#[target_feature(enable = "bmi1,bmi2,lzcnt")]
pub(crate) unsafe fn encode_4x(codes: &Codes, src: &[u8], out: &mut Vec<u8>) {
    let table = entries(codes);
    let segment = src.len().div_ceil(4);
    // the jump table, each stream at its longest with its marker, and the
    // 8 bytes a flush stores
    out.reserve(6 + src.len() * MAX_BITS as usize / 8 + 4 + 8);
    let start = out.len();
    // safety: the reservation covers every store; all bytes up to the end
    // of the last stream are written before the length is set
    unsafe {
        let base = out.as_mut_ptr().add(start);
        let mut at = base.add(6);
        for (i, part) in src.chunks(segment).enumerate() {
            let end = match codes.log {
                11 => stream::<5, true>(&table, part, at),
                10 => stream::<5, false>(&table, part, at),
                9 => stream::<6, true>(&table, part, at),
                8 => stream::<7, true>(&table, part, at),
                7 => stream::<8, true>(&table, part, at),
                _ => stream::<9, false>(&table, part, at),
            };
            if i < 3 {
                let len = end.offset_from(at) as u16;
                base.add(2 * i).cast::<u16>().write_unaligned(len.to_le());
            }
            at = end;
        }
        out.set_len(at.offset_from(out.as_ptr()) as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::super::huf;

    /// Both writers must produce the same bytes: frames are held to
    /// digests recorded once for all processors.
    #[test]
    fn writes_what_the_portable_writer_writes() {
        if !crate::has_bmi2() {
            println!("skipped: this processor lacks BMI2");
            return;
        }
        let mut state = 0x243F_6A88_85A3_08D3u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut logs = [false; 12];
        for case in 0..400 {
            // alphabets from a few symbols to all, skewed more or less
            let alphabet = 2 + next() as usize % [6, 30, 100, 255][case % 4];
            let skew = 1 + case % 5;
            let len = 256 + next() as usize % 5000;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    let mut symbol = next() as usize % alphabet;
                    for _ in 0..skew {
                        symbol = symbol.min(next() as usize % alphabet);
                    }
                    symbol as u8
                })
                .collect();
            let mut counts = [0u32; 256];
            for &byte in &data {
                counts[byte as usize] += 1;
            }
            if counts.iter().filter(|&&count| count != 0).count() < 2 {
                continue;
            }
            let fits = (alphabet as u32).next_power_of_two().ilog2() + 1;
            let codes = huf::build(&counts, (5 + case as u32 % 7).max(fits).min(11));
            logs[codes.log as usize] = true;
            let (mut portable, mut scratch, mut ours) = (vec![1, 2, 3], Vec::new(), vec![1, 2, 3]);
            huf::encode_4x_any(&codes, &data, &mut scratch, &mut portable);
            // safety: BMI2 was asserted above, and the codes were built
            // from the counts of this data
            unsafe { super::encode_4x(&codes, &data, &mut ours) };
            assert!(portable == ours, "case {case}, longest code {}", codes.log);
        }
        // every group size must have been exercised
        assert!(logs[5..=11].iter().all(|&seen| seen), "{logs:?}");
    }
}
