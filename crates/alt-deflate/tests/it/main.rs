mod golden;
mod text;

use std::sync::OnceLock;

use golden::Tally;

/// xorshift64*
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| (self.next() >> 32) as u8).collect()
    }

    fn below(&mut self, bound: usize) -> usize {
        ((self.next() >> 16) % bound as u64) as usize
    }

    /// bytes drawn from the first `alphabet` byte values
    fn narrow(&mut self, len: usize, alphabet: usize) -> Vec<u8> {
        (0..len).map(|_| self.below(alphabet) as u8).collect()
    }
}

/// two mebibytes of generated text
fn text() -> &'static [u8] {
    static TEXT: OnceLock<Vec<u8>> = OnceLock::new();
    TEXT.get_or_init(|| text::generate(0x7e87, 2 << 20))
}

const EDGE_LENGTHS: [usize; 24] = [
    0, 1, 2, 3, 4, 257, 258, 259, 260, 4095, 4096, 4097, 32767, 32768, 32769, 65534, 65535, 65536,
    65537, 131071, 131072, 131073, 196605, 196606,
];

#[test]
fn edge_lengths() {
    let mut tally = Tally::new();
    let random = Rng(1).bytes(200_000);
    let text = text();
    let zeros = vec![0u8; 200_000];
    for len in EDGE_LENGTHS {
        tally.check(&random[..len]);
        tally.check(&text[..len]);
        tally.check(&zeros[..len]);
    }
    tally.finish(
        "edge_lengths",
        "09e6dc3932286c70571157706a408348d1ab6f183402b6fba46d0db0b1c23272",
    );
}

#[test]
fn zeros_and_patterns() {
    let mut tally = Tally::new();
    tally.check(&vec![0u8; 1 << 20]);
    tally.check(&vec![0xffu8; 70_000]);
    let mut rng = Rng(2);
    for (period, total) in [
        (1, 1000),
        (2, 70_000),
        (3, 100_000),
        (7, 300_000),
        (300, 400_000),
        (40_000, 500_000),
        (32_768, 300_000),
        (32_506, 300_000),
        (65_536, 400_000),
    ] {
        let unit = rng.bytes(period);
        let data: Vec<u8> = unit.iter().copied().cycle().take(total).collect();
        tally.check(&data);
    }
    let unit = &text()[..300];
    let data: Vec<u8> = unit.iter().copied().cycle().take(250_000).collect();
    tally.check(&data);
    tally.finish(
        "zeros_and_patterns",
        "98790d9b6bdfe4c05af582a1ce1d0000959e3bf45e8ae772d3e5c9768eac11d7",
    );
}

#[test]
fn random_bytes() {
    let mut tally = Tally::new();
    let mut rng = Rng(3);
    for len in (1..64).chain([
        100, 1000, 5000, 16_383, 16_384, 16_385, 50_000, 100_000, 307_200,
    ]) {
        tally.check(&rng.bytes(len));
    }
    tally.finish(
        "random_bytes",
        "7405ff20a41b265a0564ee14099600e74f89e6ba95f7034a77a7a3b30818a9dc",
    );
}

#[test]
fn text_whole_and_chunks() {
    let mut tally = Tally::new();
    let text = text();
    tally.check(text);
    for chunk in text.chunks(150_000) {
        tally.check(chunk);
    }
    tally.finish(
        "text_whole_and_chunks",
        "5e43360d6153cff7507d773e0591fefa252cc5e0cc44fcba8bddd2805deb246a",
    );
}

#[test]
fn text_prefixes() {
    let mut tally = Tally::new();
    let text = text();
    for len in 0..=2000 {
        tally.check(&text[..len]);
    }
    let mut len = 2001;
    while len < text.len() {
        tally.check(&text[..len]);
        len += len / 3 + 17;
    }
    tally.finish(
        "text_prefixes",
        "9912da2e79a274386f4fa228de85d19ba864d47f7ffb2e140c913f76e891314a",
    );
}

#[test]
fn text_suffix_windows() {
    let mut tally = Tally::new();
    let text = text();
    let mut rng = Rng(4);
    for _ in 0..60 {
        let start = rng.below(text.len() - 100_000);
        let len = rng.below(100_000);
        tally.check(&text[start..start + len]);
    }
    tally.finish(
        "text_suffix_windows",
        "d3e38d908ecd4e5e9d9f47a188457b9cccd4ee95a293fff75575cafb52a40f58",
    );
}

#[test]
fn mixed_regions() {
    let mut tally = Tally::new();
    let text = text();
    let mut rng = Rng(5);
    for _ in 0..6 {
        let mut data = Vec::new();
        while data.len() < 400_000 {
            let len = 1 + rng.below(30_000);
            match rng.below(4) {
                0 => data.extend_from_slice(&rng.bytes(len)),
                1 => {
                    let start = rng.below(text.len() - len);
                    data.extend_from_slice(&text[start..start + len]);
                }
                2 => data.extend(std::iter::repeat_n(rng.next() as u8, len)),
                _ => data.extend_from_slice(&rng.narrow(len, 4)),
            }
        }
        tally.check(&data);
    }
    tally.finish(
        "mixed_regions",
        "a04c784538861702d6f3426cfb0aecf148748c3c98793557a5b3d1d2cdff1838",
    );
}

#[test]
fn low_entropy() {
    let mut tally = Tally::new();
    let mut rng = Rng(6);
    for alphabet in [2, 3, 4, 8, 16, 60] {
        for len in [10, 1000, 40_000, 300_000] {
            tally.check(&rng.narrow(len, alphabet));
        }
    }
    // skewed frequencies push the code lengths to their limits
    let mut data = Vec::new();
    let (mut a, mut b) = (1usize, 1usize);
    for symbol in 0..24u8 {
        data.extend(std::iter::repeat_n(symbol, a));
        (a, b) = (b, a + b);
    }
    for seed in 0..4 {
        let mut shuffled = data.clone();
        let mut rng = Rng(100 + seed);
        for i in (1..shuffled.len()).rev() {
            shuffled.swap(i, rng.below(i + 1));
        }
        tally.check(&shuffled);
    }
    tally.finish(
        "low_entropy",
        "beafde62a4a96bf0d4701defdc269d7ffeb0520466aab73cbbb56e522f7da293",
    );
}

#[test]
fn long_distance_repeats() {
    let mut tally = Tally::new();
    let mut rng = Rng(7);
    let text = text();
    for distance in (32_768 - 270..=32_768 - 255).chain(32_760..=32_776) {
        for phrase in [rng.bytes(400), text[5000..5400].to_vec()] {
            // the phrase, incompressible filler, the phrase again `distance` later
            for lead in [0, 1, 2, 300, 40_000] {
                let mut data = rng.bytes(lead);
                data.extend_from_slice(&phrase);
                data.extend_from_slice(&rng.bytes(distance - phrase.len()));
                data.extend_from_slice(&phrase);
                data.extend_from_slice(&rng.bytes(50));
                tally.check(&data);
            }
        }
    }
    tally.finish(
        "long_distance_repeats",
        "7f0a341cea7d3827825c8b9e93023f44fbaff3f0887b83e04b3f9f2afaa4a46d",
    );
}

#[test]
fn gzip_and_the_writer_wrap_the_same_stream() {
    use std::io::Write;
    let data: Vec<u8> = (0..50_000u32)
        .flat_map(|n| (n % 251).to_le_bytes())
        .collect();
    for level in [0u8, 1, 6, 9] {
        let raw = alt_deflate::compress_as_zlib(&data, level, alt_deflate::Format::Raw);
        let gzip = alt_deflate::compress_as_zlib(&data, level, alt_deflate::Format::Gzip);
        assert_eq!(&gzip[10..gzip.len() - 8], raw, "level {level}");
        let (content, consumed) =
            alt_inflate::inflate(alt_inflate::Format::Gzip, &gzip, data.len()).unwrap();
        assert_eq!(content, data, "level {level}");
        assert_eq!(consumed, gzip.len(), "level {level}");

        let mut writer = alt_deflate::Writer::new(Vec::new(), level, alt_deflate::Format::Zlib);
        for piece in data.chunks(777) {
            writer.write_all(piece).unwrap();
        }
        assert_eq!(writer.total_in(), data.len() as u64);
        assert_eq!(
            writer.finish().unwrap(),
            alt_deflate::compress(&data, level, alt_deflate::Format::Zlib),
            "level {level}"
        );
    }
}

/// the general compressor is free in what it emits, so it is checked by decoding
#[test]
fn the_general_compressor_round_trips() {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut noise = |len: usize, alphabet: u64| -> Vec<u8> {
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ((state >> 24) % alphabet) as u8
            })
            .collect()
    };
    let source = &text()[..7568];
    let mut inputs: Vec<Vec<u8>> = vec![
        Vec::new(),
        vec![1],
        vec![1, 2, 3],
        vec![9; 4],
        vec![0; 300_000],
        b"abcdefghij".repeat(30_000),
        noise(200_000, 256),
        noise(200_000, 4),
        source.repeat(40),
    ];
    for len in [
        2usize, 4, 5, 7, 8, 9, 257, 258, 259, 4096, 32767, 32768, 32769, 65536, 100_000,
    ] {
        inputs.push(source.iter().cycle().take(len).copied().collect());
    }
    let mut mixed = Vec::new();
    for round in 0..60 {
        mixed.extend(noise(1500 + round * 13, 256));
        mixed.extend_from_slice(&source[round * 31..round * 31 + 2500]);
        mixed.extend(vec![round as u8; 400]);
        // a repeat from just inside and just outside the window
        let back = if round % 2 == 0 { 32768 } else { 32769 };
        if mixed.len() > back + 300 {
            let from = mixed.len() - back;
            let copy = mixed[from..from + 300].to_vec();
            mixed.extend(copy);
        }
    }
    inputs.push(mixed);
    for (index, data) in inputs.iter().enumerate() {
        for level in 0..=9u8 {
            for (ours, theirs) in [
                (alt_deflate::Format::Raw, alt_inflate::Format::Raw),
                (alt_deflate::Format::Zlib, alt_inflate::Format::Zlib),
                (alt_deflate::Format::Gzip, alt_inflate::Format::Gzip),
            ] {
                let stream = alt_deflate::compress(data, level, ours);
                let what = format!(
                    "input {index} ({} bytes), level {level}, {ours:?}",
                    data.len()
                );
                let (content, consumed) =
                    alt_inflate::inflate(theirs, &stream, data.len()).expect(&what);
                assert!(content == *data, "{what}: content differs");
                assert_eq!(consumed, stream.len(), "{what}");
            }
            // never much larger than what zlib makes of it at its fastest level
            let reference = alt_deflate::compress_as_zlib(data, 1, alt_deflate::Format::Raw).len();
            let ours = alt_deflate::compress(data, level.max(1), alt_deflate::Format::Raw).len();
            assert!(
                ours <= reference + reference / 4 + 64,
                "input {index}, level {level}: {ours} bytes against {reference}"
            );
        }
    }
}
