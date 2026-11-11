//! Inputs whose Huffman codes run into the format's length limits: fifteen bits for
//! literals and distances, seven for the code that describes the other two.

use super::Rng;

fn round_trips(data: &[u8], what: &str) {
    for level in 0..=9 {
        for (name, stream) in [
            (
                "general",
                alt_deflate::compress(data, level, alt_deflate::Format::Zlib),
            ),
            (
                "as zlib",
                alt_deflate::compress_as_zlib(data, level, alt_deflate::Format::Zlib),
            ),
        ] {
            let back = alt_inflate::inflate(alt_inflate::Format::Zlib, &stream, data.len());
            assert!(
                back.is_ok_and(|(back, _)| back == data),
                "{what}, {name}, level {level}"
            );
        }
    }
}

/// a tree object the fast levels once described with code lengths no decoder accepts
#[test]
fn a_block_whose_length_code_is_cut_to_seven_bits() {
    round_trips(include_bytes!("limits/tree-3600.raw"), "tree object");
}

/// byte values whose counts grow like the Fibonacci numbers make the deepest tree
#[test]
fn skewed_symbol_counts() {
    let mut rng = Rng(0x5eed_1234_abcd_ef01);
    for symbols in 2..=28 {
        let mut data = Vec::new();
        let (mut a, mut b) = (1usize, 1usize);
        for symbol in 0..symbols {
            data.extend(std::iter::repeat_n(symbol as u8 * 7 + 1, a.min(60_000)));
            (a, b) = (b, a + b);
        }
        // shuffled, so that matching does not swallow the runs
        for at in (1..data.len()).rev() {
            data.swap(at, rng.below(at + 1));
        }
        round_trips(&data, &format!("{symbols} skewed symbols"));
    }
}

/// many short blocks of varied shape: each draws its own alphabet and skew
#[test]
fn varied_small_inputs() {
    let mut rng = Rng(0x0dd_ba11_f00d_cafe);
    for case in 0..4000 {
        let len = 1 + rng.below(6000);
        let alphabet = 2 + rng.below(254);
        let skew = 1 + rng.below(6);
        let data: Vec<u8> = (0..len)
            .map(|_| {
                // the minimum of several draws favours the low values more and more
                let draw = (0..skew).map(|_| rng.below(alphabet)).min().unwrap();
                (draw as u8).wrapping_mul(37)
            })
            .collect();
        round_trips(&data, &format!("case {case}"));
    }
}
