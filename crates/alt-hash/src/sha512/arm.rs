//! SHA-512 compression with the ARMv8.2 SHA-512 instructions.

use super::K;
use std::arch::aarch64::*;

/// Two rounds per `sha512h` / `sha512h2` pair; the four state vectors hold
/// (a b), (c d), (e f), (g h) and change roles after every pair.
#[target_feature(enable = "sha3")]
pub(super) unsafe fn compress(state: &mut [u64; 8], blocks: &[u8]) {
    // loads and stores stay inside `state`, `K` and the 128-byte block
    unsafe {
        let mut v = [
            vld1q_u64(state.as_ptr()),
            vld1q_u64(state.as_ptr().add(2)),
            vld1q_u64(state.as_ptr().add(4)),
            vld1q_u64(state.as_ptr().add(6)),
        ];
        for block in blocks.as_chunks::<128>().0 {
            let entry = v;
            let mut s = [vdupq_n_u64(0); 8];
            for (i, slot) in s.iter_mut().enumerate() {
                *slot = vreinterpretq_u64_u8(vrev64q_u8(vld1q_u8(block.as_ptr().add(16 * i))));
            }
            for i in 0..40 {
                let [ab, cd, ef, gh] = v;
                let keyed = vaddq_u64(s[i & 7], vld1q_u64(K.as_ptr().add(2 * i)));
                let sum = vaddq_u64(vextq_u64(keyed, keyed, 1), gh);
                let mid = vsha512hq_u64(sum, vextq_u64(ef, gh, 1), vextq_u64(cd, ef, 1));
                v = [vsha512h2q_u64(mid, cd, ab), ab, vaddq_u64(cd, mid), ef];
                if i < 32 {
                    s[i & 7] = vsha512su1q_u64(
                        vsha512su0q_u64(s[i & 7], s[(i + 1) & 7]),
                        s[(i + 7) & 7],
                        vextq_u64(s[(i + 4) & 7], s[(i + 5) & 7], 1),
                    );
                }
            }
            for (slot, before) in v.iter_mut().zip(entry) {
                *slot = vaddq_u64(*slot, before);
            }
        }
        for (i, value) in v.into_iter().enumerate() {
            vst1q_u64(state.as_mut_ptr().add(2 * i), value);
        }
    }
}
