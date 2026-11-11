//! ChaCha20 (RFC 8439 section 2.4) and HChaCha20.

#[cfg(target_arch = "x86_64")]
mod avx2;
#[cfg(target_arch = "aarch64")]
mod neon;

const CONSTANTS: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

fn words<const N: usize>(bytes: &[u8]) -> [u32; N] {
    let mut out = [0u32; N];
    for (slot, word) in out.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *slot = u32::from_le_bytes(*word);
    }
    out
}

fn initial(key: &[u8; 32], nonce: &[u8; 12], counter: u32) -> [u32; 16] {
    let mut state = [0u32; 16];
    state[..4].copy_from_slice(&CONSTANTS);
    state[4..12].copy_from_slice(&words::<8>(key));
    state[12] = counter;
    state[13..].copy_from_slice(&words::<3>(nonce));
    state
}

macro_rules! quarter {
    ($s:ident, $a:expr, $b:expr, $c:expr, $d:expr) => {
        $s[$a] = $s[$a].wrapping_add($s[$b]);
        $s[$d] = ($s[$d] ^ $s[$a]).rotate_left(16);
        $s[$c] = $s[$c].wrapping_add($s[$d]);
        $s[$b] = ($s[$b] ^ $s[$c]).rotate_left(12);
        $s[$a] = $s[$a].wrapping_add($s[$b]);
        $s[$d] = ($s[$d] ^ $s[$a]).rotate_left(8);
        $s[$c] = $s[$c].wrapping_add($s[$d]);
        $s[$b] = ($s[$b] ^ $s[$c]).rotate_left(7);
    };
}

/// the twenty rounds, without the final addition of the input
fn permute(mut s: [u32; 16]) -> [u32; 16] {
    for _ in 0..10 {
        quarter!(s, 0, 4, 8, 12);
        quarter!(s, 1, 5, 9, 13);
        quarter!(s, 2, 6, 10, 14);
        quarter!(s, 3, 7, 11, 15);
        quarter!(s, 0, 5, 10, 15);
        quarter!(s, 1, 6, 11, 12);
        quarter!(s, 2, 7, 8, 13);
        quarter!(s, 3, 4, 9, 14);
    }
    s
}

fn keystream(state: &[u32; 16]) -> [u8; 64] {
    let mixed = permute(*state);
    let mut out = [0u8; 64];
    for (i, chunk) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *chunk = mixed[i].wrapping_add(state[i]).to_le_bytes();
    }
    out
}

/// One keystream block.
pub fn block(key: &[u8; 32], nonce: &[u8; 12], counter: u32) -> [u8; 64] {
    keystream(&initial(key, nonce, counter))
}

/// The HChaCha20 subkey for `key` and the first 16 bytes of an XChaCha20 nonce.
pub fn hchacha20(key: &[u8; 32], nonce: &[u8; 16]) -> [u8; 32] {
    let mut state = [0u32; 16];
    state[..4].copy_from_slice(&CONSTANTS);
    state[4..12].copy_from_slice(&words::<8>(key));
    state[12..].copy_from_slice(&words::<4>(nonce));
    let mixed = permute(state);
    let mut out = [0u8; 32];
    for (chunk, word) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(mixed[..4].iter().chain(&mixed[12..]))
    {
        *chunk = word.to_le_bytes();
    }
    out
}

/// xors the keystream into whole groups of blocks, advances the counter in
/// `state` and returns how many bytes were handled
type Wide = unsafe fn(&mut [u32; 16], &mut [u8]) -> usize;

/// the fastest multi-block implementation this CPU supports, and its name
fn select() -> (&'static str, Option<Wide>) {
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        return ("neon", Some(neon::apply));
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        return ("avx2", Some(avx2::apply));
    }
    ("portable", None)
}

/// which implementation runs on this CPU
pub fn implementation() -> &'static str {
    select().0
}

/// Xors `data` with the keystream that starts at block `counter`.
pub fn apply(key: &[u8; 32], nonce: &[u8; 12], counter: u32, data: &mut [u8]) {
    let mut state = initial(key, nonce, counter);
    let mut done = 0;
    if let Some(wide) = select().1 {
        // an implementation is only selected after its CPU features were detected
        done = unsafe { wide(&mut state, data) };
    }
    tail(&mut state, &mut data[done..]);
}

/// The reference implementation every accelerated one is compared with.
pub fn apply_portable(key: &[u8; 32], nonce: &[u8; 12], counter: u32, data: &mut [u8]) {
    tail(&mut initial(key, nonce, counter), data);
}

fn tail(state: &mut [u32; 16], data: &mut [u8]) {
    for chunk in data.chunks_mut(64) {
        let stream = keystream(state);
        state[12] = state[12].wrapping_add(1);
        for (byte, key) in chunk.iter_mut().zip(stream) {
            *byte ^= key;
        }
    }
}
