//! Decodes the sequences of a block and executes them into the output.

use crate::Error;
use crate::bits::BackReader;
use crate::literals::Literals;
use crate::sequences::SequenceDecoder;

/// Bytes a wide copy may write past the end of what it was asked to copy.
pub(crate) const OUT_SLACK: usize = 32;

/// Where sequences write and what they may copy from: the output of the
/// current frame, then `prefix` before it.
pub(crate) struct Sink<'a> {
    pub out: &'a mut Vec<u8>,
    pub frame_start: usize,
    pub prefix: &'a [u8],
    /// most the block may add to `out`
    pub max_len: usize,
    /// what to report when the block would add more
    pub overflow: Error,
}

impl SequenceDecoder {
    /// Decodes `count` sequences from `stream` and appends what they produce
    /// to the sink, followed by the literals no sequence used.
    pub(crate) fn execute(
        &self,
        stream: &[u8],
        count: usize,
        literals: Literals,
        sink: Sink<'_>,
        repeats: &mut [usize; 3],
    ) -> Result<(), Error> {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("bmi2") {
            // safety: the processor was just found to have the feature
            return unsafe { self.execute_bmi2(stream, count, literals, sink, repeats) };
        }
        self.execute_any(stream, count, literals, sink, repeats)
    }

    /// The same loop compiled for processors with BMI2, where the variable
    /// shifts of bit reading need no flags and no fixed register.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "bmi2")]
    unsafe fn execute_bmi2(
        &self,
        stream: &[u8],
        count: usize,
        literals: Literals,
        sink: Sink<'_>,
        repeats: &mut [usize; 3],
    ) -> Result<(), Error> {
        self.execute_any(stream, count, literals, sink, repeats)
    }

    #[inline(always)]
    fn execute_any(
        &self,
        stream: &[u8],
        count: usize,
        literals: Literals,
        sink: Sink<'_>,
        repeats: &mut [usize; 3],
    ) -> Result<(), Error> {
        let Sink {
            out,
            frame_start,
            prefix,
            max_len,
            overflow,
        } = sink;
        let mut reader = BackReader::new(stream)?;
        let mut ll_state = reader.read(self.logs[0]) as usize;
        let mut of_state = (1 << 9) + reader.read(self.logs[1]) as usize;
        let mut ml_state = (2 << 9) + reader.read(self.logs[2]) as usize;
        let [mut rep0, mut rep1, mut rep2] = *repeats;
        out.reserve(max_len + OUT_SLACK);

        // safety (whole block): the three states index the table array
        // inside their own table by construction of the entries; `op` never
        // passes `limit`, which leaves OUT_SLACK reserved bytes for wide
        // copies; `lit` never passes the end of the literals, which are
        // followed by LIT_SLACK readable bytes; match sources are checked
        // against the history available before any copy
        unsafe {
            let table = self.entries.as_ptr();
            let base = out.as_mut_ptr();
            let history = base.add(frame_start);
            let mut op = base.add(out.len());
            let limit = op.add(max_len);
            let mut lit = literals.ptr;
            let lit_end = lit.add(literals.len);
            let mut left = count;

            loop {
                reader.refill();
                let ll = *table.add(ll_state);
                let of = *table.add(of_state);
                let ml = *table.add(ml_state);

                let offset;
                if of.extra() > 1 {
                    offset = of.base() + reader.read(of.extra()) as usize;
                    rep2 = rep1;
                    rep1 = rep0;
                    rep0 = offset;
                } else {
                    // only literal-length code 0 has baseline 0, and it has
                    // no extra bits
                    let no_literals = ll.base() == 0;
                    if of.extra() == 0 {
                        if no_literals {
                            (rep0, rep1) = (rep1, rep0);
                        }
                    } else {
                        let which = 1 + no_literals as usize + reader.read(1) as usize;
                        let chosen = match which {
                            1 => rep1,
                            2 => rep2,
                            _ => rep0 - 1,
                        };
                        if chosen == 0 {
                            return Err(Error::Corrupt);
                        }
                        if which != 1 {
                            rep2 = rep1;
                        }
                        rep1 = rep0;
                        rep0 = chosen;
                    }
                    offset = rep0;
                }
                let match_len = ml.base() + reader.read(ml.extra()) as usize;
                if of.extra() + ml.extra() + ll.extra() >= 31 {
                    reader.refill();
                }
                let lit_len = ll.base() + reader.read(ll.extra()) as usize;
                left -= 1;
                if left != 0 {
                    ll_state = ll.next() + reader.read(ll.bits()) as usize;
                    ml_state = ml.next() + reader.read(ml.bits()) as usize;
                    of_state = of.next() + reader.read(of.bits()) as usize;
                }

                if lit_len > lit_end.offset_from(lit) as usize {
                    return Err(Error::Corrupt);
                }
                if lit_len + match_len > limit.offset_from(op) as usize {
                    return Err(overflow);
                }
                op.copy_from_nonoverlapping(lit, 32);
                if lit_len > 32 {
                    copy_wide(lit.add(32), op.add(32), lit_len - 32);
                }
                lit = lit.add(lit_len);
                op = op.add(lit_len);

                if offset > op.offset_from(history) as usize {
                    copy_from_prefix(op, history, prefix, offset, match_len)?;
                } else {
                    copy_match(op, offset, match_len);
                }
                op = op.add(match_len);
                if left == 0 {
                    break;
                }
            }

            let rest = lit_end.offset_from(lit) as usize;
            if rest > limit.offset_from(op) as usize {
                return Err(overflow);
            }
            op.copy_from_nonoverlapping(lit, rest);
            op = op.add(rest);
            out.set_len(op.offset_from(base) as usize);
        }
        reader.refill();
        if !reader.finished() {
            return Err(Error::Corrupt);
        }
        *repeats = [rep0, rep1, rep2];
        Ok(())
    }
}

/// Copies a match that starts before the output of this frame, in `prefix`,
/// and may run on into the frame.
///
/// # Safety
/// `history <= op`, both in one buffer that is valid for `len + 31` bytes of
/// writes at `op`, and `offset` larger than their distance.
#[cold]
unsafe fn copy_from_prefix(
    op: *mut u8,
    history: *const u8,
    prefix: &[u8],
    offset: usize,
    len: usize,
) -> Result<(), Error> {
    // safety: `reach` bytes are read from the end of the prefix after the
    // check against its length; the rest of the match then starts exactly
    // at `history`, which trails the write position by `offset`
    unsafe {
        let reach = offset - op.offset_from(history) as usize;
        if reach > prefix.len() {
            return Err(Error::Corrupt);
        }
        let from = prefix.as_ptr().add(prefix.len() - reach);
        if len <= reach {
            op.copy_from_nonoverlapping(from, len);
        } else {
            op.copy_from_nonoverlapping(from, reach);
            copy_match(op.add(reach), offset, len - reach);
        }
    }
    Ok(())
}

/// Copies `len` bytes in 16-byte steps; reads and writes may run up to 15
/// bytes past `len`.
///
/// # Safety
/// Both ranges, rounded up to a multiple of 16, must be valid, and the
/// destination must not start less than 16 bytes after the source.
#[inline(always)]
unsafe fn copy_wide(src: *const u8, dst: *mut u8, len: usize) {
    // safety: the caller guarantees the rounded-up ranges; each step reads
    // 16 bytes that no later step of this call has yet to write
    unsafe {
        let mut at = 0;
        while at < len {
            dst.add(at).copy_from_nonoverlapping(src.add(at), 16);
            at += 16;
        }
    }
}

/// How far to move the source after the first four bytes of a close-range
/// match, and how far back to step it after the next four, so that source
/// and destination end up at least 8 bytes apart on a multiple of the offset.
const NEAR_FORWARD: [usize; 8] = [0, 1, 2, 1, 4, 4, 4, 4];
const NEAR_BACK: [usize; 8] = [8, 8, 8, 7, 8, 9, 10, 11];

/// Copies a match of `len` bytes that starts `offset` bytes before `op`,
/// replicating the pattern when the two overlap. May write up to 31 bytes
/// past `len`.
///
/// # Safety
/// `offset` must be nonzero, the `offset` bytes before `op` initialized, and
/// `op` valid for `len + 31` bytes of writes.
#[inline(always)]
unsafe fn copy_match(op: *mut u8, offset: usize, len: usize) {
    // safety: sources always trail the write position, so every byte read
    // was either initialized before the call or written by it
    unsafe {
        let mut src = op.sub(offset) as *const u8;
        if offset >= 16 {
            // 32 bytes up front: most matches end there, and a branch on
            // the length would often be mispredicted
            op.copy_from_nonoverlapping(src, 16);
            op.add(16).copy_from_nonoverlapping(src.add(16), 16);
            if len > 32 {
                copy_wide(src.add(32), op.add(32), len - 32);
            }
            return;
        }
        let mut dst = op;
        let end = op.add(len);
        if offset < 8 {
            for i in 0..4 {
                dst.add(i).write(src.add(i).read());
            }
            src = src.add(NEAR_FORWARD[offset]);
            dst.add(4).copy_from_nonoverlapping(src, 4);
            // net movement is forwards; the intermediate value may lie
            // before the buffer, hence the wrapping arithmetic
            src = src.wrapping_sub(NEAR_BACK[offset]).wrapping_add(8);
        } else {
            dst.copy_from_nonoverlapping(src, 8);
            src = src.add(8);
        }
        dst = dst.add(8);
        while dst < end {
            dst.copy_from_nonoverlapping(src, 8);
            src = src.add(8);
            dst = dst.add(8);
        }
    }
}
