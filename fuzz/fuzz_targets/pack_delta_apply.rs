//! Git delta application on adversarial input. The delta header carries an
//! attacker-controlled target size; resolving a delta from an untrusted pack
//! must neither panic nor pre-allocate that size unbounded (decompression
//! bomb). The harness splits the input into (base, delta).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    // first byte picks the base/delta split point
    let split = data[0] as usize % data.len();
    let (base, delta) = data[1..].split_at(split.min(data.len() - 1));
    let _ = alt_git_pack::delta::apply(base, delta);
});
