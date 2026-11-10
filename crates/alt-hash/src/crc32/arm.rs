//! CRC-32 on AArch64 by carry-less multiplication and the CRC instructions together.
//!
//! The message is kept as several 128-bit remainders ("streams") that each stand for a
//! share of the data read so far. One step multiplies every stream ahead by the
//! distance of all of them and adds the next blocks. The multiplier and the CRC
//! instruction use different execution units, so on long inputs three CRC chains run
//! over stretches of their own while the streams skip those, and are folded in after.

use super::ahead;
use std::arch::aarch64::*;

/// the multipliers that move a stream ahead by this many bytes
const fn keys(bytes: u32) -> [u64; 2] {
    ahead(bytes * 8)
}

/// `a` moved ahead by the distance of `by`, plus `b`
#[inline(always)]
unsafe fn fold(a: uint64x2_t, b: uint64x2_t, by: uint64x2_t) -> uint64x2_t {
    unsafe {
        let low = vmull_p64(vgetq_lane_u64::<0>(a), vgetq_lane_u64::<0>(by));
        let high = vmull_high_p64(vreinterpretq_p64_u64(a), vreinterpretq_p64_u64(by));
        // two plain exclusive ors: the first one pairs with a multiplication, which the
        // three-way instruction would prevent
        veorq_u64(
            b,
            veorq_u64(vreinterpretq_u64_p128(low), vreinterpretq_u64_p128(high)),
        )
    }
}

/// the next 16 bytes of `data`
#[inline(always)]
unsafe fn load(data: &mut &[u8]) -> uint64x2_t {
    let (block, rest) = data.split_first_chunk::<16>().unwrap();
    *data = rest;
    unsafe { vreinterpretq_u64_u8(vld1q_u8(block.as_ptr())) }
}

#[inline(always)]
fn word(data: &[u8], index: usize) -> u64 {
    u64::from_le_bytes(data[index * 8..index * 8 + 8].try_into().unwrap())
}

/// a register value as the stream it is equivalent to at the same place
#[inline(always)]
unsafe fn seed(register: u32) -> uint64x2_t {
    unsafe { vsetq_lane_u64::<0>(register as u64, vdupq_n_u64(0)) }
}

#[inline(always)]
pub(super) fn supported() -> bool {
    std::arch::is_aarch64_feature_detected!("aes") && std::arch::is_aarch64_feature_detected!("crc")
}

/// advances the register (without inversion) over `data`
#[inline]
#[target_feature(enable = "neon,aes,crc")]
pub(super) unsafe fn fold_bytes(register: u32, data: &[u8]) -> u32 {
    unsafe {
        match data.len() {
            0..64 => chain(register, data),
            64..256 => streams::<4>(register, data, false),
            256..1024 => streams::<8>(register, data, false),
            _ => long(register, data),
        }
    }
}

/// Kept out of line: the short paths above are small enough to live in the caller,
/// these are not, and at these lengths a call costs nothing.
#[inline(never)]
#[target_feature(enable = "neon,aes,crc")]
unsafe fn long(register: u32, data: &[u8]) -> u32 {
    unsafe {
        if data.len() < 256 + mixed_bytes(4) {
            return streams::<8>(register, data, false);
        }
        // The chains wait for every word they read, so they only pay off on data that is
        // already in the first-level cache: measured on an Apple M4, they add 15% up
        // to the size of that cache and cost 7% beyond it. Asked before the streams
        // are loaded, because a call in between would push them all to the stack.
        let mixed = data.len() <= crate::cache::first_level_data_bytes();
        streams::<16>(register, data, mixed)
    }
}

/// one chain of CRC instructions
#[inline(always)]
unsafe fn chain(mut register: u32, data: &[u8]) -> u32 {
    let (words, mut rest) = data.as_chunks::<8>();
    unsafe {
        for word in words {
            register = __crc32d(register, u64::from_le_bytes(*word));
        }
        if let Some((four, tail)) = rest.split_first_chunk::<4>() {
            register = __crc32w(register, u32::from_le_bytes(*four));
            rest = tail;
        }
        if let Some((two, tail)) = rest.split_first_chunk::<2>() {
            register = __crc32h(register, u16::from_le_bytes(*two));
            rest = tail;
        }
        if let Some(byte) = rest.first() {
            register = __crc32b(register, *byte);
        }
    }
    register
}

/// bytes of a mixed chunk of `steps` steps
const fn mixed_bytes(steps: usize) -> usize {
    (3 * 32 + 256) * steps
}

/// One mixed chunk of `STEPS` steps. Each step folds the sixteen streams over 256
/// bytes and advances each of three CRC chains by four words, which take about the
/// same time. The chains' stretches come first in the chunk; the streams skip them
/// with their first fold, and the chains' results are folded in at the end.
#[inline(always)]
unsafe fn mixed_chunk<const STEPS: usize>(v: &mut [uint64x2_t; 16], data: &mut &[u8]) {
    unsafe {
        let chain = 32 * STEPS;
        let (first, rest) = data.split_at(chain);
        let (second, rest) = rest.split_at(chain);
        let (third, rest) = rest.split_at(chain);
        let (mut folded, rest) = rest.split_at(256 * STEPS);
        *data = rest;
        let (mut a, mut b, mut c) = (0u32, 0u32, 0u32);
        let mut by = vld1q_u64(const { keys((256 + 3 * 32 * STEPS) as u32) }.as_ptr());
        for step in 0..STEPS {
            for stream in v.iter_mut() {
                *stream = fold(*stream, load(&mut folded), by);
            }
            by = vld1q_u64(const { keys(256) }.as_ptr());
            for index in step * 4..step * 4 + 4 {
                a = __crc32d(a, word(first, index));
                b = __crc32d(b, word(second, index));
                c = __crc32d(c, word(third, index));
            }
        }
        // A chain's result belongs right after its stretch; move each from there to
        // where the first stream stands now, 256 bytes before the end of the chunk.
        let chains = veorq_u64(
            veorq_u64(
                vreinterpretq_u64_p128(vmull_p64(
                    a as u64,
                    const { keys((2 * 32 * STEPS + 256 * STEPS - 256) as u32)[0] },
                )),
                vreinterpretq_u64_p128(vmull_p64(
                    b as u64,
                    const { keys((32 * STEPS + 256 * STEPS - 256) as u32)[0] },
                )),
            ),
            vreinterpretq_u64_p128(vmull_p64(
                c as u64,
                const { keys((256 * STEPS - 256) as u32)[0] },
            )),
        );
        v[0] = veorq_u64(v[0], chains);
    }
}

/// `N` streams (4, 8 or 16) over at least `16 * N` bytes; `mixed` (with sixteen
/// streams only) lets the CRC chains take their share
#[inline(always)]
unsafe fn streams<const N: usize>(register: u32, mut data: &[u8], mixed: bool) -> u32 {
    unsafe {
        let mut v = [vdupq_n_u64(0); N];
        for stream in v.iter_mut() {
            *stream = load(&mut data);
        }
        v[0] = veorq_u64(v[0], seed(register));
        if mixed {
            let v: &mut [uint64x2_t; 16] = (&mut v[..]).try_into().unwrap();
            while data.len() >= mixed_bytes(16) {
                mixed_chunk::<16>(v, &mut data);
            }
            while data.len() >= mixed_bytes(8) {
                mixed_chunk::<8>(v, &mut data);
            }
            while data.len() >= mixed_bytes(4) {
                mixed_chunk::<4>(v, &mut data);
            }
        }
        let by = vld1q_u64(const { keys(16 * N as u32) }.as_ptr());
        while data.len() >= 16 * N {
            for stream in v.iter_mut() {
                *stream = fold(*stream, load(&mut data), by);
            }
        }
        // Fewer than 16 * N bytes are left. Fold the first half of the streams onto the
        // second, which costs one level of latency per halving, and let the remaining
        // half take more input when there is enough.
        macro_rules! halve {
            ($half:literal) => {
                if N >= 2 * $half {
                    let by = vld1q_u64(const { keys(16 * $half) }.as_ptr());
                    for index in 0..$half {
                        v[index] = fold(v[index], v[index + $half], by);
                    }
                    if data.len() >= 16 * $half {
                        for stream in v[..$half].iter_mut() {
                            *stream = fold(*stream, load(&mut data), by);
                        }
                    }
                }
            };
        }
        halve!(8);
        halve!(4);
        halve!(2);
        halve!(1);
        // the last stream stands for the message so far: its own CRC from zero is the
        // register after that message
        let register = __crc32d(0, vgetq_lane_u64::<0>(v[0]));
        let register = __crc32d(register, vgetq_lane_u64::<1>(v[0]));
        // often nothing is left, and skipping the tail is a quarter of a 64-byte call
        if data.is_empty() {
            return register;
        }
        chain(register, data)
    }
}
