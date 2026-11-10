mod oracle;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use oracle::check;

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

fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "target") {
                rust_files(&path, found);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// the workspace's own sources, concatenated in path order
fn text() -> &'static [u8] {
    static TEXT: OnceLock<Vec<u8>> = OnceLock::new();
    TEXT.get_or_init(|| {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut files = Vec::new();
        rust_files(crates, &mut files);
        let mut text = Vec::new();
        for file in files {
            text.extend_from_slice(&std::fs::read(file).unwrap());
            if text.len() >= 2 << 20 {
                break;
            }
        }
        assert!(text.len() >= 1 << 20, "only {} bytes of text", text.len());
        text
    })
}

fn report(class: &str, inputs: usize, compared: usize) {
    println!(
        "{class}: {inputs} inputs, {compared} comparisons against libz {}",
        oracle::version()
    );
}

/// counts inputs and comparisons of one test
#[derive(Default)]
struct Tally {
    inputs: usize,
    compared: usize,
}

impl Tally {
    fn check(&mut self, what: &str, data: &[u8]) {
        self.inputs += 1;
        self.compared += check(what, data);
    }

    fn report(&self, class: &str) {
        report(class, self.inputs, self.compared);
    }
}

const EDGE_LENGTHS: [usize; 24] = [
    0, 1, 2, 3, 4, 257, 258, 259, 260, 4095, 4096, 4097, 32767, 32768, 32769, 65534, 65535, 65536,
    65537, 131071, 131072, 131073, 196605, 196606,
];

#[test]
fn edge_lengths() {
    let mut tally = Tally::default();
    let random = Rng(1).bytes(200_000);
    let text = text();
    let zeros = vec![0u8; 200_000];
    for len in EDGE_LENGTHS {
        tally.check(&format!("random prefix {len}"), &random[..len]);
        tally.check(&format!("text prefix {len}"), &text[..len]);
        tally.check(&format!("zeros {len}"), &zeros[..len]);
    }
    tally.report("edge lengths");
}

#[test]
fn zeros_and_patterns() {
    let mut tally = Tally::default();
    tally.check("zeros 1 MiB", &vec![0u8; 1 << 20]);
    tally.check("0xff 70000", &vec![0xffu8; 70_000]);
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
        tally.check(&format!("random unit of {period} repeated"), &data);
    }
    let unit = &text()[..300];
    let data: Vec<u8> = unit.iter().copied().cycle().take(250_000).collect();
    tally.check("text unit of 300 repeated", &data);
    tally.report("zeros and patterns");
}

#[test]
fn random_bytes() {
    let mut tally = Tally::default();
    let mut rng = Rng(3);
    for len in (1..64).chain([
        100, 1000, 5000, 16_383, 16_384, 16_385, 50_000, 100_000, 307_200,
    ]) {
        tally.check(&format!("random {len}"), &rng.bytes(len));
    }
    tally.report("random");
}

#[test]
fn text_whole_and_chunks() {
    let mut tally = Tally::default();
    let text = text();
    tally.check("text", text);
    for (n, chunk) in text.chunks(150_000).enumerate() {
        tally.check(&format!("text chunk {n}"), chunk);
    }
    tally.report("text");
}

#[test]
fn text_prefixes() {
    let mut tally = Tally::default();
    let text = text();
    for len in 0..=2000 {
        tally.check(&format!("text prefix {len}"), &text[..len]);
    }
    let mut len = 2001;
    while len < text.len() {
        tally.check(&format!("text prefix {len}"), &text[..len]);
        len += len / 3 + 17;
    }
    tally.report("text prefixes");
}

#[test]
fn text_suffix_windows() {
    let mut tally = Tally::default();
    let text = text();
    let mut rng = Rng(4);
    for _ in 0..60 {
        let start = rng.below(text.len() - 100_000);
        let len = rng.below(100_000);
        tally.check(
            &format!("text {start}..{}", start + len),
            &text[start..start + len],
        );
    }
    tally.report("text windows");
}

#[test]
fn mixed_regions() {
    let mut tally = Tally::default();
    let text = text();
    let mut rng = Rng(5);
    for round in 0..6 {
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
        tally.check(&format!("mixed regions round {round}"), &data);
    }
    tally.report("mixed");
}

#[test]
fn low_entropy() {
    let mut tally = Tally::default();
    let mut rng = Rng(6);
    for alphabet in [2, 3, 4, 8, 16, 60] {
        for len in [10, 1000, 40_000, 300_000] {
            tally.check(
                &format!("alphabet of {alphabet}, {len} bytes"),
                &rng.narrow(len, alphabet),
            );
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
        tally.check(&format!("fibonacci frequencies, shuffle {seed}"), &shuffled);
    }
    tally.report("low entropy");
}

#[test]
fn long_distance_repeats() {
    let mut tally = Tally::default();
    let mut rng = Rng(7);
    let text = text();
    for distance in (32_768 - 270..=32_768 - 255).chain(32_760..=32_776) {
        for (kind, phrase) in [
            ("random", rng.bytes(400)),
            ("text", text[5000..5400].to_vec()),
        ] {
            // the phrase, incompressible filler, the phrase again `distance` later
            for lead in [0, 1, 2, 300, 40_000] {
                let mut data = rng.bytes(lead);
                data.extend_from_slice(&phrase);
                data.extend_from_slice(&rng.bytes(distance - phrase.len()));
                data.extend_from_slice(&phrase);
                data.extend_from_slice(&rng.bytes(50));
                tally.check(
                    &format!("{kind} phrase repeated {distance} back after {lead}"),
                    &data,
                );
            }
        }
    }
    tally.report("long-distance repeats");
}

#[test]
fn gzip_and_the_writer_wrap_the_same_stream() {
    use std::io::Write;
    let data: Vec<u8> = (0..50_000u32)
        .flat_map(|n| (n % 251).to_le_bytes())
        .collect();
    for level in [0u8, 1, 6, 9] {
        let raw = alt_deflate::compress(&data, level, alt_deflate::Format::Raw);
        let gzip = alt_deflate::compress(&data, level, alt_deflate::Format::Gzip);
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
