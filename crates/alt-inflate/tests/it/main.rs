use alt_inflate::{Error, Format, Inflater, Reader, inflate, inflate_exact};
use flate2::{Compression, write};
use std::io::{Read, Write};

fn noise(len: usize, mut state: u64, alphabet: u8) -> Vec<u8> {
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8 % alphabet.max(1)
        })
        .collect()
}

/// source text of this repository: realistic compressible input
fn text() -> Vec<u8> {
    let mut out = Vec::new();
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    for krate in ["alt-inflate", "alt-hash", "alt-pack-segment"] {
        let mut files: Vec<_> = std::fs::read_dir(format!("{root}/{krate}/src"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .collect();
        files.sort();
        for file in files {
            out.extend(std::fs::read(file).unwrap());
        }
    }
    out
}

fn corpus() -> Vec<(String, Vec<u8>)> {
    let text = text();
    let mut inputs = vec![
        ("empty".to_string(), Vec::new()),
        ("one byte".to_string(), vec![7]),
        ("zeros".to_string(), vec![0; 300_000]),
        ("pattern".to_string(), b"abcdefg".repeat(20_000)),
        ("random".to_string(), noise(200_000, 3, 0)),
        ("few symbols".to_string(), noise(200_000, 5, 5)),
        ("text".to_string(), text.clone()),
    ];
    let mut mixed = Vec::new();
    for round in 0..40 {
        mixed.extend(noise(3000, round, 0));
        mixed.extend(&text[round as usize * 1000..][..5000]);
        mixed.extend(vec![round as u8; 700]);
    }
    inputs.push(("mixed".to_string(), mixed));
    for len in [
        1usize, 2, 3, 10, 100, 257, 258, 259, 1000, 32767, 32768, 32769, 65536,
    ] {
        inputs.push((format!("text prefix {len}"), text[..len].to_vec()));
    }
    inputs
}

fn compress(format: Format, level: u32, data: &[u8]) -> Vec<u8> {
    let level = Compression::new(level);
    match format {
        Format::Raw => {
            let mut encoder = write::DeflateEncoder::new(Vec::new(), level);
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        }
        Format::Zlib => {
            let mut encoder = write::ZlibEncoder::new(Vec::new(), level);
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        }
        Format::Gzip => {
            let mut encoder = write::GzEncoder::new(Vec::new(), level);
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        }
    }
}

#[test]
fn whole_streams_decode_and_report_their_length() {
    for (name, data) in corpus() {
        for format in [Format::Raw, Format::Zlib, Format::Gzip] {
            for level in [0, 1, 6, 9] {
                let mut stream = compress(format, level, &data);
                let length = stream.len();
                // bytes after the stream do not belong to it
                stream.extend_from_slice(b"trailing bytes that are not part of it");
                let what = format!("{name}, {format:?}, level {level}");
                let (content, consumed) = inflate(format, &stream, usize::MAX).expect(&what);
                assert_eq!(content, data, "{what}");
                assert_eq!(consumed, length, "{what}");
                let mut exact = vec![0; data.len()];
                assert_eq!(
                    inflate_exact(format, &stream, &mut exact),
                    Ok(length),
                    "{what}"
                );
                assert_eq!(exact, data, "{what}");
            }
        }
    }
}

#[test]
fn any_split_of_input_and_output_gives_the_same_content() {
    for (name, data) in corpus() {
        if data.len() > 70_000 && name != "text" && name != "mixed" {
            continue;
        }
        for (format, level) in [
            (Format::Zlib, 1),
            (Format::Gzip, 6),
            (Format::Raw, 9),
            (Format::Zlib, 0),
        ] {
            let stream = compress(format, level, &data);
            for seed in 1..6u64 {
                let sizes = noise(4096, seed, 0);
                let mut inflater = Inflater::new(format);
                let mut content = Vec::new();
                let (mut given, mut held, mut turn) = (0, 0, 0);
                let mut done = false;
                while !done {
                    // input arrives in pieces; what was not consumed is offered again
                    let piece = match seed {
                        1 => 1,
                        2 => 1 + sizes[turn % 4096] as usize % 7,
                        _ => 1 + sizes[turn % 4096] as usize * seed as usize,
                    };
                    given = (given + piece).min(stream.len());
                    let room = match seed {
                        1 | 3 => 1 + sizes[(turn + 1) % 4096] as usize % 5,
                        _ => 1 + sizes[(turn + 1) % 4096] as usize * 40,
                    };
                    turn += 1;
                    let mut output = vec![0; room];
                    let step = inflater.inflate(&stream[held..given], &mut output).unwrap();
                    held += step.consumed;
                    content.extend_from_slice(&output[..step.produced]);
                    done = step.done;
                    assert!(turn < 10_000_000, "{name}: no progress");
                }
                let what = format!("{name}, {format:?}, level {level}, split {seed}");
                assert_eq!(content, data, "{what}");
                assert_eq!(held, stream.len(), "{what}");
                assert_eq!(inflater.total_in(), stream.len() as u64, "{what}");
                assert_eq!(inflater.total_out(), data.len() as u64, "{what}");
            }
        }
    }
}

#[test]
fn damaged_streams_are_refused_without_panicking() {
    let data = text();
    let data = &data[..40_000];
    for format in [Format::Raw, Format::Zlib, Format::Gzip] {
        let stream = compress(format, 6, data);
        let flips = noise(4000, 11, 0);
        for round in 0..2000 {
            let mut damaged = stream.clone();
            let at = (flips[round] as usize * 251 + flips[round + 1] as usize * 17 + round * 7)
                % damaged.len();
            damaged[at] ^= 1 << (flips[round + 2] % 8);
            // either an error, or (a flip the format cannot see) some content
            let _ = inflate(format, &damaged, 1 << 20);
        }
        for cut in (0..stream.len()).step_by(97) {
            assert_eq!(
                inflate(format, &stream[..cut], 1 << 20).err(),
                Some(Error::Truncated),
                "{format:?} cut at {cut}"
            );
        }
        let mut wrong = stream.clone();
        let last = wrong.len() - 1;
        wrong[last] ^= 0x40;
        if format != Format::Raw {
            assert_eq!(
                inflate(format, &wrong, 1 << 20).err(),
                Some(Error::Checksum)
            );
        }
    }
    let stream = compress(Format::Zlib, 6, data);
    assert_eq!(
        inflate(Format::Zlib, &stream, 39_999).err(),
        Some(Error::TooLong)
    );
    let mut short = vec![0; 39_999];
    assert_eq!(
        inflate_exact(Format::Zlib, &stream, &mut short).err(),
        Some(Error::TooLong)
    );
    let mut long = vec![0; 40_001];
    assert_eq!(
        inflate_exact(Format::Zlib, &stream, &mut long).err(),
        Some(Error::TooShort)
    );
}

#[test]
fn readers_follow_one_stream_or_every_gzip_member() {
    let data = text();
    let (first, second) = data.split_at(data.len() / 3);
    let mut file = compress(Format::Gzip, 6, first);
    let one = file.len();
    file.extend(compress(Format::Gzip, 1, second));

    let mut single = Reader::new(Format::Gzip, &file[..]);
    let mut content = Vec::new();
    single.read_to_end(&mut content).unwrap();
    assert_eq!(content, first);
    assert_eq!(single.total_in(), one as u64);

    let mut every = Reader::gzip_members(&file[..]);
    let mut content = Vec::new();
    every.read_to_end(&mut content).unwrap();
    assert_eq!(content, data);

    // a source that hands out a few bytes at a time
    struct Trickle<'a>(&'a [u8]);
    impl Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let take = self.0.len().min(out.len()).min(3);
            out[..take].copy_from_slice(&self.0[..take]);
            self.0 = &self.0[take..];
            Ok(take)
        }
    }
    let mut slow = Reader::gzip_members(Trickle(&file));
    let mut content = Vec::new();
    slow.read_to_end(&mut content).unwrap();
    assert_eq!(content, data);

    let mut cut = Reader::new(Format::Gzip, &file[..one - 3]);
    let error = cut.read_to_end(&mut Vec::new()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
}
