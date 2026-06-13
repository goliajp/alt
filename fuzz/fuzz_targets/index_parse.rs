//! Git index parsing on adversarial input. The index is untrusted content
//! (a `.alt`/`.git` on disk can be hand-edited); parsing must never panic.
//! The trailer is forced valid so the fuzzer reaches the entry/extension
//! logic rather than bouncing off the checksum, and the magic is set so it
//! gets past the header.
#![no_main]

use alt_git_codec::HashAlgo;
use alt_git_index::Index;
use libfuzzer_sys::fuzz_target;
use sha1::{Digest, Sha1};

fuzz_target!(|data: &[u8]| {
    if data.len() < 12 {
        return;
    }
    let mut buf = data.to_vec();
    buf[..4].copy_from_slice(b"DIRC");
    let trailer = Sha1::digest(&buf);
    buf.extend_from_slice(&trailer);
    let _ = Index::parse(&buf, HashAlgo::Sha1);
});
