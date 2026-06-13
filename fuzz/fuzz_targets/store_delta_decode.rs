//! Lineage delta decode on arbitrary input: a corrupt stored record must
//! make the zstd ref-prefix decoder return None, never panic or hang. The
//! declared original length is a u32 record field in practice, so the
//! harness caps it to a realistic span rather than letting the allocator
//! limit dominate the fuzz signal.
#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let base: Vec<u8> = Vec::arbitrary(&mut u).unwrap_or_default();
    let payload: Vec<u8> = Vec::arbitrary(&mut u).unwrap_or_default();
    // real records carry a u32 orig_len; cap to 8 MiB so the fuzzer exercises
    // decode logic, not the allocator's reaction to a 4 GiB reservation
    let orig_len = (u32::arbitrary(&mut u).unwrap_or(0) as usize) % (8 << 20);
    let _ = alt_store::delta::decompress_with_base(&payload, &base, orig_len);
});
