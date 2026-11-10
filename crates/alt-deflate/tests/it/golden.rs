//! Every output of the zlib-identical compressor for a test's inputs, folded into
//! one digest and held against the digest of the bytes zlib produces for them.

use alt_deflate::Format;
use alt_hash::Sha256;

/// folds the outputs of one test
pub struct Tally {
    hasher: Sha256,
    inputs: usize,
    outputs: usize,
}

impl Tally {
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            inputs: 0,
            outputs: 0,
        }
    }

    /// compresses `data` at every level in both framings
    pub fn check(&mut self, data: &[u8]) {
        self.inputs += 1;
        for level in 0..=9u8 {
            for (tag, format) in [(0u8, Format::Raw), (1, Format::Zlib)] {
                let output = alt_deflate::compress_as_zlib(data, level, format);
                self.hasher.update(&[level, tag]);
                self.hasher.update(&(output.len() as u64).to_le_bytes());
                self.hasher.update(&output);
                self.outputs += 1;
            }
        }
    }

    pub fn finish(self, test: &str, expected: &str) {
        let digest: String = self
            .hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert!(
            digest == expected,
            "{test}: digest {digest} over {} outputs of {} inputs, expected {expected}",
            self.outputs,
            self.inputs
        );
    }
}
