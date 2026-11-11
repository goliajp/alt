//! Deterministic inputs shared by the tests.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, bound: usize) -> usize {
        ((self.next() >> 32) as usize) % bound
    }
}

pub fn random(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    (0..len).map(|_| (rng.next() >> 56) as u8).collect()
}

/// bytes drawn from a small alphabet: compressible by entropy coding only
pub fn few_symbols(len: usize, seed: u64, alphabet: usize) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    (0..len).map(|_| b'a' + rng.below(alphabet) as u8).collect()
}

/// prose-like text: a skewed vocabulary, punctuation and line breaks
pub fn text(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let words: Vec<String> = (0..600)
        .map(|_| {
            let n = 2 + rng.below(9);
            (0..n)
                .map(|_| (b'a' + rng.below(26) as u8) as char)
                .collect()
        })
        .collect();
    let mut out = Vec::with_capacity(len + 16);
    while out.len() < len {
        // squaring skews the choice towards the first words
        let pick = rng.below(words.len()) * rng.below(words.len()) / words.len();
        out.extend_from_slice(words[pick].as_bytes());
        out.extend_from_slice(match rng.below(20) {
            0 => b". ",
            1 => b", ",
            2 => b"\n",
            _ => b" ",
        });
    }
    out.truncate(len);
    out
}

/// fixed-layout records: counters, small enums, zero padding, short blobs
pub fn records(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(len + 64);
    let mut id = 1000u32;
    while out.len() < len {
        id += 1 + rng.below(3) as u32;
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&(rng.below(7) as u16).to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&((rng.below(1000) as f64) * 0.25).to_le_bytes());
        let blob = rng.below(12);
        out.push(blob as u8);
        for _ in 0..blob {
            out.push((rng.next() >> 56) as u8);
        }
    }
    out.truncate(len);
    out
}

/// long runs of one byte broken up by short noise
pub fn runs(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(len + 70_000);
    while out.len() < len {
        let byte = rng.below(4) as u8;
        let run = 1 + rng.below(70_000);
        out.resize(out.len() + run, byte);
        for _ in 0..rng.below(5) {
            out.push((rng.next() >> 56) as u8);
        }
    }
    out.truncate(len);
    out
}

/// text, then well over a megabyte of cheap filler, then pieces of the
/// text again: matches whose offsets reach far back
pub fn far_repeats(seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = text(64 << 10, seed);
    out.extend_from_slice(&runs(1_500_000, seed + 1));
    for _ in 0..200 {
        let from = rng.below(60 << 10);
        let len = 30 + rng.below(3000);
        let piece = out[from..from + len].to_vec();
        out.extend_from_slice(&piece);
    }
    out
}

/// a copy with scattered edits: bytes changed, pieces inserted and removed
pub fn mutate(base: &[u8], seed: u64, edits: usize) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = base.to_vec();
    for _ in 0..edits {
        if out.is_empty() {
            break;
        }
        let at = rng.below(out.len());
        match rng.below(3) {
            0 => out[at] ^= 1 + rng.below(255) as u8,
            1 => {
                let piece = random(1 + rng.below(40), rng.next());
                out.splice(at..at, piece);
            }
            _ => {
                let end = (at + 1 + rng.below(40)).min(out.len());
                out.drain(at..end);
            }
        }
    }
    out
}

/// bytes skewed towards small values over the whole range: entropy coding
/// helps, matching does not
pub fn skewed(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    (0..len)
        .map(|_| {
            let word = rng.next();
            ((word >> 56) as u8)
                .min((word >> 48) as u8)
                .min((word >> 40) as u8)
        })
        .collect()
}

/// a noise period repeated, each copy replacing a few more bytes with one
/// fixed value; a greedy matcher then sees that value as its only literal.
/// One replacement sits on the first byte of the third block so that the
/// block does not open with a stray literal.
pub fn stitched(seed: u64) -> Vec<u8> {
    const PERIOD: usize = 4096;
    let mut period: Vec<u8> = random(PERIOD, seed);
    for byte in &mut period {
        *byte = (*byte).min(0xFE);
    }
    let mut out = period.clone();
    for copy in 1..73 {
        for slot in (0..PERIOD / 64).filter(|slot| (slot + copy) % 2 == 0) {
            period[64 * slot + copy % 64] = 0xFF;
        }
        out.extend_from_slice(&period);
    }
    out.extend_from_slice(&period[..100]);
    out
}

/// one changing byte, then eight fixed ones: every sequence has the same
/// literal length, match length and offset
pub fn uniform_sequences(seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    let mut last = 0;
    for _ in 0..40_000 {
        // never the same byte twice in a row, or a match would run on
        last = (last + 1 + rng.below(255)) % 256;
        out.push(last as u8);
        out.extend_from_slice(b"ABCDEFGH");
    }
    out
}

/// literal runs and matches of every length class: each unit is a run of
/// fresh noise followed by a copy of older output that starts at the head
/// of an earlier run, where a greedy matcher is sure to have looked. Long
/// units start on a block boundary so that no block end cuts them short;
/// the gap before them is filled with one long copy, which costs nothing.
/// The noise draws on 32 symbols: it holds no repeat a fast matcher would
/// find, but shrinks under entropy coding, or blocks made mostly of it
/// would be stored raw.
pub fn length_sweep(seed: u64) -> Vec<u8> {
    const BLOCK: usize = 128 << 10;
    let mut rng = Rng::new(seed);
    // every small length, then each power of two and a point inside its
    // class; of the costly long literal runs just one per class
    let mut lit_lens: Vec<usize> = (0..=70).collect();
    let mut match_lens: Vec<usize> = (4..=140).collect();
    for k in 7..=16 {
        let power = 1usize << k;
        if k < 13 {
            lit_lens.extend([power, power + power / 2]);
        } else {
            lit_lens.push(power + power / 8);
        }
        match_lens.extend([power + 3, power + power / 2 + 3]);
    }
    let mut units = Vec::new();
    for lit_len in lit_lens {
        units.push((lit_len, 16 + rng.below(48)));
    }
    for match_len in match_lens {
        units.push((rng.below(16), match_len));
    }

    let mut noise = Rng::new(seed + 1);
    let mut fresh = move || b'A' + (noise.next() >> 59) as u8;
    let mut out: Vec<u8> = (0..64).map(|_| fresh()).collect();
    let mut heads = vec![0];
    for (lit_len, match_len) in units {
        if lit_len + match_len >= 8192 {
            for i in 0..(BLOCK - out.len() % BLOCK) % BLOCK {
                out.push(out[i]);
            }
        }
        heads.push(out.len());
        out.extend((0..lit_len).map(|_| fresh()));
        // a recent head with enough behind it, so the copy stays in reach
        // of small windows; failing that, the oldest
        let at = out.len();
        let from = heads
            .iter()
            .rev()
            .copied()
            .find(|&head| head + match_len <= at && at - head <= 300_000 + match_len)
            .unwrap_or(0);
        let piece = out[from..(from + match_len).min(at)].to_vec();
        out.extend_from_slice(&piece);
    }
    out
}

/// Shortest corpus input whose compressed size is compared with the
/// reference's.
pub const SIZED_MIN: usize = 100_000;

pub fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    let mut mixed = text(140_000, 21);
    mixed.extend_from_slice(&records(140_000, 22));
    mixed.extend_from_slice(&random(30_000, 23));
    mixed.extend_from_slice(&runs(200_000, 24));
    mixed.extend_from_slice(&few_symbols(140_000, 25, 5));
    vec![
        ("empty", Vec::new()),
        ("one byte", vec![0x5A]),
        ("two bytes", vec![1, 2]),
        ("tiny text", b"hello hello hello hello".to_vec()),
        ("zeros", vec![0; 300_000]),
        ("zeros short", vec![0; 1000]),
        ("period 7", b"abcdefg".repeat(40_000)),
        (
            "period 1 then 3",
            [vec![9u8; 50], b"xyz".repeat(3000)].concat(),
        ),
        ("text tiny", text(200, 13)),
        ("skewed noise", skewed(2000, 14)),
        ("stitched periods", stitched(15)),
        ("uniform sequences", uniform_sequences(16)),
        ("text small", text(3000, 1)),
        ("text block", text(60_000, 2)),
        ("text multi-block", text(280_000, 3)),
        ("random", random(12_000, 4)),
        ("random small", random(500, 5)),
        ("few symbols", few_symbols(140_000, 6, 4)),
        ("two symbols", few_symbols(70_000, 7, 2)),
        ("records", records(200_000, 8)),
        ("runs", runs(700_000, 9)),
        ("exactly one block", text(128 << 10, 10)),
        ("one block and a byte", text((128 << 10) + 1, 11)),
        ("mixed", mixed),
        ("far repeats", far_repeats(12)),
        ("length sweep", length_sweep(17)),
    ]
}

/// (name, prefix, data) for the tests of compression against a prefix
pub fn prefixed() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let page = text(60_000, 41);
    let binary = records(150_000, 42);
    let big = [text(300_000, 43), records(300_000, 44)].concat();
    vec![
        ("similar text", page.clone(), mutate(&page, 1, 40)),
        ("similar binary", binary.clone(), mutate(&binary, 2, 120)),
        ("identical", page.clone(), page.clone()),
        ("unrelated", random(50_000, 45), text(40_000, 46)),
        ("empty prefix", Vec::new(), text(40_000, 47)),
        ("empty data", page.clone(), Vec::new()),
        ("both empty", Vec::new(), Vec::new()),
        ("tiny data", page.clone(), page[30_000..30_050].to_vec()),
        (
            "tiny prefix",
            b"abcdefgh".to_vec(),
            b"abcdefghabcdefgh-abcdefgh".repeat(40),
        ),
        (
            "prefix shorter than a probe",
            b"abc".to_vec(),
            b"abcabcabcabc".to_vec(),
        ),
        (
            "prefix much larger",
            big.clone(),
            mutate(&big[400_000..470_000], 3, 30),
        ),
        (
            "data much larger",
            page[..5000].to_vec(),
            mutate(&big[..200_000], 4, 100),
        ),
        ("large both", big.clone(), mutate(&big, 5, 400)),
        (
            "match across the seam",
            page.clone(),
            [&page[59_000..], &page[..500]].concat(),
        ),
    ]
}

/// (length, data, prefix): short inputs of every length up to a few
/// hundred bytes and around the sizes where headers change shape
pub fn small() -> Vec<(usize, Vec<u8>, Vec<u8>)> {
    let mut rng = Rng::new(91);
    let mut lens: Vec<usize> = (0..300).collect();
    lens.extend([
        1023, 1024, 1025, 4095, 4096, 4097, 16383, 16384, 16385, 65535, 65536, 65791, 65792,
    ]);
    lens.into_iter()
        .map(|len| {
            let seed = rng.next();
            let data = match len % 6 {
                0 => text(len, seed),
                1 => random(len, seed),
                2 => few_symbols(len, seed, 3),
                3 => records(len, seed),
                4 => runs(len, seed),
                _ => skewed(len, seed),
            };
            let prefix = mutate(&data, seed, 3);
            (len, data, prefix)
        })
        .collect()
}

/// (name, data) of the frames the damage tests start from
pub fn damage_sources() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("text", text(6000, 61)),
        ("records", records(5000, 62)),
        ("runs", runs(40_000, 63)),
        ("random", random(700, 64)),
        ("multi-block", text(133_000, 65)),
        ("stitched", stitched(66)),
    ]
}

/// (prefix, data): a megabyte taken from the far start of a prefix of tens
/// of megabytes, with scattered edits
pub fn far_reach() -> (Vec<u8>, Vec<u8>) {
    let prefix = [records(20 << 20, 51), text(20 << 20, 52)].concat();
    let data = mutate(&prefix[1000..(1 << 20) + 1000], 7, 200);
    (prefix, data)
}
