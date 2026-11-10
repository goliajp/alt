//! The decoding loop for the middle of a block, where neither the input nor the
//! output is close to its end: bits are taken eight bytes at a time and matches are
//! copied in words, without a check per byte.

use crate::{
    Error,
    blocks::{Bits, Tables},
    huffman::{LITERAL, POINTER, SPECIAL},
};

/// input the loop may read past its position in one round: two refills of eight bytes
const INPUT_MARGIN: usize = 16;
/// output a match may touch past its own end: the word-sized copy overruns
const COPY_MARGIN: usize = 8;

pub(crate) fn worthwhile(bits: &Bits<'_>, output: &[u8], written: usize) -> bool {
    bits.input.len() - bits.at >= INPUT_MARGIN && output.len() - written > COPY_MARGIN
}

/// Decodes until the block ends (true), or until the input or output gets too close
/// to its end or a match needs output from an earlier call (false). `earlier` is how
/// much such output exists.
pub(crate) fn run(
    tables: &Tables,
    bits: &mut Bits<'_>,
    output: &mut [u8],
    written: &mut usize,
    earlier: usize,
) -> Result<bool, Error> {
    let input = bits.input;
    let input_end = input.len() - INPUT_MARGIN;
    // literals are written below this; a match checks its own end
    let literal_end = output.len() - 2;
    let match_end = output.len() - COPY_MARGIN;
    let (mut at, mut hold, mut count) = (bits.at, bits.hold, bits.count);
    let mut out = *written;
    // Refilling leaves real upcoming bits above `count`; they are masked off again
    // when the loop hands back to the careful decoder. `at` is at most `input_end`
    // when a round starts and moves at most seven bytes per refill.
    macro_rules! refill {
        () => {
            // within the input by the margin the loop keeps
            hold |=
                unsafe { input.as_ptr().add(at).cast::<u64>().read_unaligned() }.to_le() << count;
            at += ((63 - count) >> 3) as usize;
            count |= 56;
        };
    }
    macro_rules! literal {
        ($entry:expr) => {
            // below `literal_end`, checked when the round started and after each
            unsafe { *output.get_unchecked_mut(out) = ($entry >> 16) as u8 };
            out += 1;
        };
    }
    let mut ended = false;
    let result = loop {
        if at > input_end || out >= literal_end {
            break Ok(());
        }
        refill!();
        let mut entry = tables.litlen[(hold & tables.litlen_mask) as usize];
        // up to two literals straight from the first-level table: at most 22 of the
        // 56 bits, which leaves enough for any length code after them
        if entry & LITERAL != 0 {
            hold >>= entry & 0x3f;
            count -= entry & 0x3f;
            literal!(entry);
            entry = tables.litlen[(hold & tables.litlen_mask) as usize];
            if entry & LITERAL != 0 {
                hold >>= entry & 0x3f;
                count -= entry & 0x3f;
                literal!(entry);
                continue;
            }
            if out >= literal_end {
                break Ok(());
            }
        }
        // the state before this symbol, to return to if it cannot be finished here
        let before = (hold, count);
        if entry & POINTER != 0 {
            hold >>= entry & 0x3f;
            count -= entry & 0x3f;
            let index = hold & ((1 << (entry >> 8 & 0x1f)) - 1);
            entry = tables.litlen[(entry >> 16) as usize + index as usize];
        }
        hold >>= entry & 0x3f;
        count -= entry & 0x3f;
        if entry & LITERAL != 0 {
            literal!(entry);
            continue;
        }
        if entry & SPECIAL != 0 {
            if entry >> 16 != 0 {
                break Err(Error::Corrupt("undefined literal or length code"));
            }
            ended = true;
            break Ok(());
        }
        let extra = entry >> 8 & 0x1f;
        let length = (entry >> 16) as usize + (hold & ((1 << extra) - 1)) as usize;
        hold >>= extra;
        count -= extra;
        // a distance takes up to 28 bits
        if count < 28 {
            refill!();
        }
        let mut entry = tables.distance[(hold & tables.distance_mask) as usize];
        if entry & POINTER != 0 {
            hold >>= entry & 0x3f;
            count -= entry & 0x3f;
            let index = hold & ((1 << (entry >> 8 & 0x1f)) - 1);
            entry = tables.distance[(entry >> 16) as usize + index as usize];
        }
        if entry & SPECIAL != 0 {
            break Err(Error::Corrupt("undefined distance code"));
        }
        hold >>= entry & 0x3f;
        count -= entry & 0x3f;
        let extra = entry >> 8 & 0x1f;
        let distance = (entry >> 16) as usize + (hold & ((1 << extra) - 1)) as usize;
        hold >>= extra;
        count -= extra;
        if distance > out || out + length > match_end {
            if distance > out + earlier {
                break Err(Error::Corrupt("match reaches before the start of the data"));
            }
            // the source is in an earlier call's output, or the end of the output is
            // too near for a copy by words: leave it to the careful decoder
            (hold, count) = before;
            break Ok(());
        }
        copy(output, out, distance, length);
        out += length;
    };
    // whole bytes above `count` were counted as read by the refills; `give_back` in
    // the caller returns them
    bits.at = at;
    bits.hold = hold & ((1u64 << count) - 1);
    bits.count = count;
    *written = out;
    result.map(|()| ended)
}

/// Copies a match of `length` bytes from `distance` back. The caller guarantees room
/// for the match plus a word after it.
#[inline(always)]
fn copy(output: &mut [u8], out: usize, distance: usize, length: usize) {
    debug_assert!(distance <= out && out + length + 8 <= output.len());
    // within bounds by the margin the loop keeps; a word at a time may write up to
    // seven bytes past the match, which later output overwrites
    unsafe {
        let to = output.as_mut_ptr().add(out);
        let from = to.sub(distance);
        if distance >= 8 {
            let mut done = 0;
            while done < length {
                to.add(done)
                    .cast::<u64>()
                    .write_unaligned(from.add(done).cast::<u64>().read_unaligned());
                done += 8;
            }
        } else if distance == 1 {
            to.write_bytes(*from, length);
        } else {
            for done in 0..length {
                *to.add(done) = *from.add(done);
            }
        }
    }
}
