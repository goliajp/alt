//! Plain frames: those of the reference implementation are decoded from
//! the packs, our own are decoded again and held to their recorded digests.

use std::collections::{HashMap, HashSet};

use alt_zstd::{decode_all, decode_with_prefix, encode_all, encode_with_prefix};

use crate::inputs::{corpus, random, small, text};
use crate::walk::{self, Shape};
use crate::{digests, pack};

/// records in `frames/corpus.pack`
const CORPUS_FRAMES: usize = 73;
/// records in `frames/small.pack`
const SMALL_FRAMES: usize = 2 * (300 * 3 + 13);
/// lines in `frames/own.sha256`: the corpus, the small inputs plain and
/// prefixed, the prefix cases and the far-reaching prefix, at two levels
const DIGESTS: usize = 2 * (26 + 2 * 313 + 14 + 1);

#[test]
fn reference_frames_decode() {
    let inputs: HashMap<_, _> = corpus().into_iter().collect();
    let records = pack::parse(pack::CORPUS);
    let mut shape = Shape::default();
    let mut seen = HashSet::new();
    for (key, frame) in &records {
        let (name, _level) = key.rsplit_once('/').unwrap();
        assert!(decode_all(frame).as_ref() == Ok(&inputs[name]), "{key}");
        walk::add(frame, &mut shape);
        seen.insert(name);
    }
    assert_eq!(seen.len(), inputs.len());
    assert_eq!(records.len(), CORPUS_FRAMES);
    assert_all_kinds(&shape, "reference encoder");
}

#[test]
fn own_frames_round_trip_and_keep_their_digests() {
    let mut shape = Shape::default();
    let mut cases = 0;
    for (name, data) in corpus() {
        for level in [1, 3] {
            let frame = encode_all(&data, level);
            assert!(
                decode_all(&frame).as_ref() == Ok(&data),
                "{name} level {level}"
            );
            digests::expect(&format!("c/{name}/{level}"), &frame);
            walk::add(&frame, &mut shape);
            cases += 1;
        }
    }
    assert_eq!(cases, 52);
    assert_all_kinds(&shape, "our encoder");
}

/// a literal run and a match longer than 16 bits can count, which the
/// encoder records apart from the other lengths of their block
#[test]
fn lengths_beyond_sixteen_bits_round_trip() {
    let noise = random(70_000, 11);
    let mut long_literals = noise.clone();
    long_literals.extend_from_slice(&[0; 50_000]);
    let mut long_match = noise[..100].to_vec();
    long_match.extend_from_slice(&[7; 110_000]);
    long_match.extend_from_slice(&noise[..300]);
    let mut both_blocks = long_literals.clone();
    both_blocks.extend_from_slice(&[0; 20_000]);
    both_blocks.extend_from_slice(&long_match);
    for (name, data) in [
        ("literals", &long_literals),
        ("match", &long_match),
        ("both", &both_blocks),
    ] {
        for level in [1, 3] {
            let frame = encode_all(data, level);
            assert!(frame.len() < data.len(), "{name} level {level}");
            assert!(
                decode_all(&frame).as_ref() == Ok(data),
                "{name} level {level}"
            );
        }
    }
}

/// every block kind, literal kind, stream layout and table mode must have
/// occurred, or the corpus is not testing what it claims to
fn assert_all_kinds(shape: &Shape, who: &str) {
    println!("{who}: {shape:?}");
    assert!(shape.blocks.iter().all(|&n| n > 0), "{who}: {shape:?}");
    assert!(shape.literals.iter().all(|&n| n > 0), "{who}: {shape:?}");
    assert!(
        shape.one_stream > 0 && shape.four_streams > 0,
        "{who}: {shape:?}"
    );
    assert!(shape.without_sequences > 0, "{who}: {shape:?}");
    for field in shape.modes {
        assert!(field.iter().all(|&n| n > 0), "{who}: {shape:?}");
    }
}

#[test]
fn small_reference_frames_decode() {
    let inputs: HashMap<_, _> = small()
        .into_iter()
        .map(|(len, data, prefix)| (len.to_string(), (data, prefix)))
        .collect();
    let records = pack::parse(pack::SMALL);
    for (key, frame) in &records {
        let mut parts = key.split('/');
        let kind = parts.next().unwrap();
        let (data, prefix) = &inputs[parts.next().unwrap()];
        let plain = match kind {
            "plain" => decode_all(frame),
            _ => decode_with_prefix(frame, prefix, data.len()),
        };
        assert!(plain.as_ref() == Ok(data), "{key}");
    }
    assert_eq!(records.len(), SMALL_FRAMES);
}

#[test]
fn small_own_frames_round_trip_and_keep_their_digests() {
    let mut cases = 0;
    for (len, data, prefix) in small() {
        for level in [1, 3] {
            let frame = encode_all(&data, level);
            assert!(
                decode_all(&frame).as_ref() == Ok(&data),
                "{len} level {level}"
            );
            digests::expect(&format!("s/{len}/{level}"), &frame);
            let frame = encode_with_prefix(&data, &prefix, level);
            let plain = decode_with_prefix(&frame, &prefix, len);
            assert!(plain.as_ref() == Ok(&data), "{len} level {level} prefixed");
            digests::expect(&format!("sp/{len}/{level}"), &frame);
            cases += 2;
        }
    }
    assert_eq!(cases, 313 * 4);
}

#[test]
fn every_digest_belongs_to_a_test() {
    assert_eq!(digests::count(), DIGESTS);
}

#[test]
fn sizes_stay_close_to_reference() {
    let reference: HashMap<_, _> = pack::parse(pack::CORPUS)
        .into_iter()
        .map(|(key, frame)| (key, frame.len()))
        .collect();
    let mut compared = 0;
    for (name, data) in corpus() {
        if data.len() < 100_000 {
            continue;
        }
        // level 3 is the one level every large input has a reference
        // frame for
        let theirs = reference[format!("{name}/3").as_str()];
        let ours = encode_all(&data, 3).len();
        println!("{name}: ours {ours}, reference {theirs}");
        // allow a fixed handful of bytes for inputs that shrink to nothing.
        // The reference frames were made without telling the encoder the
        // input length, which below 256 KiB gives it larger tables and a
        // longer minimum match than it takes when it knows the length, as
        // we always do; on noise over four symbols that alone is worth
        // 3.5%, with the matches of both encoders otherwise identical
        assert!(
            ours as f64 <= theirs as f64 * 1.04 + 64.0,
            "{name}: {ours} against {theirs}"
        );
        compared += 1;
    }
    assert_eq!(compared, 13);
    let data = text(450_000, 3);
    assert!(encode_all(&data, 3).len() < encode_all(&data, 1).len());
}

#[test]
fn level_mapping() {
    let data = text(300_000, 77);
    let fast = encode_all(&data, 1);
    let default = encode_all(&data, 3);
    assert_eq!(encode_all(&data, -5), fast);
    assert_eq!(encode_all(&data, 2), fast);
    assert_eq!(encode_all(&data, 0), default);
    assert_eq!(encode_all(&data, 19), default);
}
