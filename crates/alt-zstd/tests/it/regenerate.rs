//! Rebuilds everything under `frames/` with the reference implementation.
//! Not compiled normally: add `zstd = "0.13"` to this crate's
//! dev-dependencies, then run
//! `RUSTFLAGS="--cfg alt_zstd_regenerate" cargo test --release -p alt-zstd --test it regenerate -- --nocapture`.

use std::fmt::Write;

use zstd::zstd_safe::{CCtx, CParameter, DCtx, InBuffer, OutBuffer};

use crate::digests::hex;
use crate::inputs::{self, Rng, SIZED_MIN, few_symbols, random, records, runs, text};
use crate::pack::append;

fn encode(data: &[u8], level: i32) -> Vec<u8> {
    zstd::encode_all(data, level).unwrap()
}

/// one-shot compression: the reference knows the input length, as our
/// encoder always does, and picks its parameters by it
fn encode_sized(data: &[u8], level: i32) -> Vec<u8> {
    zstd::bulk::compress(data, level).unwrap()
}

fn encode_checked(data: &[u8], level: i32) -> Vec<u8> {
    let mut encoder = zstd::Encoder::new(Vec::new(), level).unwrap();
    encoder.include_checksum(true).unwrap();
    std::io::Write::write_all(&mut encoder, data).unwrap();
    encoder.finish().unwrap()
}

fn decode(frame: &[u8]) -> Option<Vec<u8>> {
    zstd::decode_all(frame).ok()
}

/// prefix compression the way the chunk store used to ask for it; with a
/// window log, the patch-from configuration: a window that covers the
/// prefix and long-distance matching
fn encode_with_prefix(data: &[u8], prefix: &[u8], level: i32, window_log: Option<u32>) -> Vec<u8> {
    let mut context = CCtx::create();
    context
        .set_parameter(CParameter::CompressionLevel(level))
        .unwrap();
    if let Some(log) = window_log {
        context.set_parameter(CParameter::WindowLog(log)).unwrap();
        context
            .set_parameter(CParameter::EnableLongDistanceMatching(true))
            .unwrap();
    }
    context.ref_prefix(prefix).unwrap();
    let mut out = Vec::with_capacity(zstd::zstd_safe::compress_bound(data.len()));
    context.compress2(&mut out, data).unwrap();
    out
}

/// streaming decode with default parameters after `ref_prefix`
fn decode_with_prefix(frame: &[u8], prefix: &[u8], len: usize) -> Option<Vec<u8>> {
    let mut context = DCtx::create();
    context.ref_prefix(prefix).ok()?;
    let mut out = Vec::with_capacity(len);
    let mut input = InBuffer::around(frame);
    let mut output = OutBuffer::around(&mut out);
    loop {
        let remaining = context.decompress_stream(&mut output, &mut input).ok()?;
        if remaining == 0 {
            break;
        }
        if output.pos() == output.capacity() || input.pos == input.src.len() {
            return None;
        }
    }
    Some(out)
}

/// whether to keep the reference frame of a level, by its size, so that
/// the packs stay small: level 3 always, the others only while cheap
fn kept(level: i32, size: usize) -> bool {
    match level {
        3 => true,
        19 => size <= 32 << 10,
        _ => size <= 8 << 10,
    }
}

/// records our own frame's digest once the reference has decoded it
fn own(digests: &mut String, key: &str, data: &[u8], prefix: Option<&[u8]>, level: i32) {
    let frame = match prefix {
        Some(prefix) => alt_zstd::encode_with_prefix(data, prefix, level),
        None => alt_zstd::encode_all(data, level),
    };
    let plain = match prefix {
        Some(prefix) => decode_with_prefix(&frame, prefix, data.len()),
        None => decode(&frame),
    };
    assert!(
        plain.as_deref() == Some(data),
        "{key}: reference rejects our frame"
    );
    writeln!(digests, "{}  {key}", hex(&alt_hash::sha256(&frame))).unwrap();
}

fn save(name: &str, bytes: &[u8]) {
    let path = format!("{}/tests/it/frames/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::write(&path, bytes).unwrap();
    println!("{name}: {} bytes", bytes.len());
}

#[test]
fn regenerate() {
    let mut digests = String::new();

    let mut corpus = Vec::new();
    for (name, data) in inputs::corpus() {
        for level in [1, 3, 9, 19] {
            let frame = encode(&data, level);
            assert!(alt_zstd::decode_all(&frame).as_ref() == Ok(&data), "{name}");
            // the sweep needs the fastest level too: only there do its
            // long literal runs survive whole
            if kept(level, frame.len()) || (name == "length sweep" && level == 1) {
                append(&mut corpus, &format!("{name}/{level}"), &frame);
            }
        }
        for level in [1, 3] {
            own(
                &mut digests,
                &format!("c/{name}/{level}"),
                &data,
                None,
                level,
            );
        }
    }
    save("corpus.pack", &corpus);

    // what our sizes are held against: the large inputs at the default
    // level, compressed in one call
    let mut sized = Vec::new();
    for (name, data) in inputs::corpus() {
        if data.len() >= SIZED_MIN {
            let frame = encode_sized(&data, 3);
            assert!(alt_zstd::decode_all(&frame).as_ref() == Ok(&data), "{name}");
            append(&mut sized, name, &frame);
        }
    }
    save("sized.pack", &sized);

    let mut small = Vec::new();
    for (len, data, prefix) in inputs::small() {
        let kept: &[i32] = if len < 1000 { &[1, 3, 19] } else { &[3] };
        for &level in kept {
            let frame = encode(&data, level);
            assert!(alt_zstd::decode_all(&frame).as_ref() == Ok(&data), "{len}");
            append(&mut small, &format!("plain/{len}/{level}"), &frame);
            let frame = encode_with_prefix(&data, &prefix, level, None);
            let ours = alt_zstd::decode_with_prefix(&frame, &prefix, len);
            assert!(ours.as_ref() == Ok(&data), "{len} prefixed");
            append(&mut small, &format!("prefixed/{len}/{level}"), &frame);
        }
        for level in [1, 3] {
            own(
                &mut digests,
                &format!("s/{len}/{level}"),
                &data,
                None,
                level,
            );
            own(
                &mut digests,
                &format!("sp/{len}/{level}"),
                &data,
                Some(&prefix),
                level,
            );
        }
    }
    save("small.pack", &small);

    let mut prefixed = Vec::new();
    for (name, prefix, data) in inputs::prefixed() {
        for level in [1, 3, 9, 19] {
            let frame = encode_with_prefix(&data, &prefix, level, None);
            let ours = alt_zstd::decode_with_prefix(&frame, &prefix, data.len());
            assert!(ours.as_ref() == Ok(&data), "{name}");
            if kept(level, frame.len()) {
                append(&mut prefixed, &format!("{name}/{level}"), &frame);
            }
        }
        let frame = encode_with_prefix(&data, &prefix, 3, Some(27));
        let ours = alt_zstd::decode_with_prefix(&frame, &prefix, data.len());
        assert!(ours.as_ref() == Ok(&data), "{name} patch");
        if kept(19, frame.len()) {
            append(&mut prefixed, &format!("{name}/patch"), &frame);
        }
        for level in [1, 3] {
            own(
                &mut digests,
                &format!("p/{name}/{level}"),
                &data,
                Some(&prefix),
                level,
            );
        }
    }
    {
        let (prefix, data) = inputs::far_reach();
        let frame = encode_with_prefix(&data, &prefix, 3, Some(27));
        let ours = alt_zstd::decode_with_prefix(&frame, &prefix, data.len());
        assert!(ours.as_ref() == Ok(&data), "far reach");
        append(&mut prefixed, "far reach/patch", &frame);
        for level in [1, 3] {
            own(
                &mut digests,
                &format!("far/{level}"),
                &data,
                Some(&prefix),
                level,
            );
        }
    }
    save("prefixed.pack", &prefixed);

    // frames with a content checksum: the damage tests start from these,
    // and the short ones cover every tail length of the checksum
    let mut checked = Vec::new();
    for (name, data) in inputs::damage_sources() {
        let kept: &[i32] = if data.len() > 100_000 { &[1] } else { &[1, 19] };
        for &level in kept {
            let frame = encode_checked(&data, level);
            assert!(alt_zstd::decode_all(&frame).as_ref() == Ok(&data), "{name}");
            append(&mut checked, &format!("source/{name}/{level}"), &frame);
        }
    }
    let tails = random(200, 35);
    for len in 0..tails.len() {
        let frame = encode_checked(&tails[..len], 1);
        assert!(alt_zstd::decode_all(&frame).as_deref() == Ok(&tails[..len]));
        append(&mut checked, &format!("tail/{len}"), &frame);
    }
    save("checked.pack", &checked);

    save("damaged.pack", &damaged());
    save("own.sha256", digests.as_bytes());
}

/// Damaged frames that both implementations still accept, with the same
/// output: each record is the SHA-256 of that output, then the frame.
fn damaged() -> Vec<u8> {
    let sources = [
        text(1500, 71),
        records(1500, 72),
        random(300, 73),
        runs(5000, 74),
        few_symbols(2000, 75, 6),
    ];
    let mut rng = Rng::new(76);
    let mut found = Vec::new();
    for data in &sources {
        let frames = [
            encode(data, 1),
            encode(data, 19),
            alt_zstd::encode_all(data, 3),
        ];
        for frame in frames {
            let mut accepted = Vec::new();
            for at in 0..frame.len() {
                for bit in 0..8 {
                    let mut damaged = frame.clone();
                    damaged[at] ^= 1 << bit;
                    let (Ok(ours), Some(theirs)) =
                        (alt_zstd::decode_all(&damaged), decode(&damaged))
                    else {
                        continue;
                    };
                    assert!(ours == theirs, "outputs differ");
                    // a flip that changes nothing teaches nothing
                    if ours != *data {
                        accepted.push((alt_hash::sha256(&ours), damaged));
                    }
                }
            }
            // an even spread of up to twenty per frame
            let step = accepted.len().div_ceil(20).max(1);
            let offset = rng.below(step);
            found.extend(accepted.into_iter().skip(offset).step_by(step));
        }
    }
    let mut pack = Vec::new();
    for (index, (digest, frame)) in found.iter().enumerate() {
        append(
            &mut pack,
            &format!("{index}"),
            &[&digest[..], frame].concat(),
        );
    }
    println!("damaged frames kept: {}", found.len());
    pack
}
