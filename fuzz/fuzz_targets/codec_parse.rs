//! Git object parsers on arbitrary bytes: a malformed commit/tree/tag must
//! return an error, never panic. These parse untrusted repository content
//! (an imported .git is an adversarial input surface).
#![no_main]

use alt_git_codec::{Commit, HashAlgo, Tag, Tree};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = Commit::parse(data);
    let _ = Tag::parse(data);
    let _ = Tree::parse(data, HashAlgo::Sha1);
    let _ = Tree::parse(data, HashAlgo::Sha256);
});
