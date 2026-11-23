//! A version 2 operation log on adversarial input. The log file is
//! untrusted content (a `.alt` on disk can be damaged or hand-edited):
//! reading it forward, as a full check does and as a command does, and
//! finding the checkpoint to begin at from its end must never panic. A
//! valid file header is put before the input so that the fuzzer reaches
//! the blocks and frames.
#![no_main]

use alt_oplog::v2::block::FileHeader;
use alt_oplog::v2::scan::{Walk, locate};
use alt_oplog::v2::state::State;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let header = FileHeader {
        block_exp: 10,
        incompatible: 0,
        read_compatible: 0,
        compatible: 0,
        id: [0; 16],
        created_ms: 0,
    };
    let mut file = header.encode().to_vec();
    file.extend_from_slice(data);
    let layout = header.layout();
    for strict in [false, true] {
        let walk = Walk {
            layout,
            data: &file,
            base: 0,
            strict,
        };
        let _ = walk.run(&mut State::empty(), &mut |_, _, _| Ok(()));
    }
    let mut read = |at: u64, len: usize| {
        let end = (at as usize + len).min(file.len());
        Ok(file.get(at as usize..end).unwrap_or_default().to_vec())
    };
    if let Ok(mut began) = locate(&layout, file.len() as u64, &mut read) {
        let walk = Walk {
            layout,
            data: &file,
            base: 0,
            strict: false,
        };
        let _ = walk.run(&mut began, &mut |_, _, _| Ok(()));
    }
});
