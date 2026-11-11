use crate::inputs::{Rng, corpus, few_symbols, mutate, random, records, runs, skewed, text};
use crate::reference;
use crate::walk::{self, Shape};

#[test]
fn reference_reads_our_frames() {
    let mut shape = Shape::default();
    let mut cases = 0;
    for (name, data) in corpus() {
        for level in [1, 3] {
            let frame = alt_zstd::encode_all(&data, level);
            let plain = reference::decode(&frame)
                .unwrap_or_else(|error| panic!("{name} level {level}: {error}"));
            assert!(
                plain == data,
                "{name} level {level}: reference output differs"
            );
            assert!(
                alt_zstd::decode_all(&frame).as_ref() == Ok(&data),
                "{name} level {level}: own output differs"
            );
            walk::add(&frame, &mut shape);
            cases += 1;
        }
    }
    assert_eq!(cases, 52);
    assert_all_kinds(&shape, "our encoder");
}

#[test]
fn we_read_reference_frames() {
    let mut shape = Shape::default();
    let mut cases = 0;
    for (name, data) in corpus() {
        for level in [1, 3, 9, 19] {
            let frame = reference::encode(&data, level);
            assert!(
                alt_zstd::decode_all(&frame).as_ref() == Ok(&data),
                "{name} level {level}"
            );
            walk::add(&frame, &mut shape);
            cases += 1;
        }
    }
    assert_eq!(cases, 104);
    assert_all_kinds(&shape, "reference encoder");
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
fn levels_differ_and_stay_close_to_reference() {
    for (name, data) in corpus() {
        if data.len() < 100_000 {
            continue;
        }
        let ours = [1, 3].map(|level| alt_zstd::encode_all(&data, level).len());
        let theirs = [1, 3].map(|level| reference::encode(&data, level).len());
        println!("{name}: ours {ours:?}, reference {theirs:?}");
        for (ours, theirs) in ours.iter().zip(theirs) {
            // allow a fixed handful of bytes for inputs that shrink to nothing
            assert!(
                *ours as f64 <= theirs as f64 * 1.03 + 64.0,
                "{name}: {ours} against {theirs}"
            );
        }
    }
    let data = crate::inputs::text(450_000, 3);
    assert!(alt_zstd::encode_all(&data, 3).len() < alt_zstd::encode_all(&data, 1).len());
}

#[test]
fn level_mapping() {
    let data = crate::inputs::text(300_000, 77);
    let fast = alt_zstd::encode_all(&data, 1);
    let default = alt_zstd::encode_all(&data, 3);
    assert_eq!(alt_zstd::encode_all(&data, -5), fast);
    assert_eq!(alt_zstd::encode_all(&data, 2), fast);
    assert_eq!(alt_zstd::encode_all(&data, 0), default);
    assert_eq!(alt_zstd::encode_all(&data, 19), default);
}

/// Short inputs of every length up to a few hundred bytes and around the
/// sizes where headers change shape, in both directions and with a prefix.
#[test]
fn small_inputs_both_ways() {
    let mut rng = Rng::new(91);
    let mut lens: Vec<usize> = (0..300).collect();
    lens.extend([
        1023, 1024, 1025, 4095, 4096, 4097, 16383, 16384, 16385, 65535, 65536, 65791, 65792,
    ]);
    let mut cases = 0;
    for len in lens {
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
        for level in [1, 3] {
            let frame = alt_zstd::encode_all(&data, level);
            assert!(
                reference::decode(&frame).unwrap() == data,
                "{len} level {level}"
            );
            assert!(
                alt_zstd::decode_all(&frame).as_ref() == Ok(&data),
                "{len} level {level}"
            );
            let frame = alt_zstd::encode_with_prefix(&data, &prefix, level);
            let plain = reference::decode_with_prefix(&frame, &prefix, len);
            assert!(
                plain.as_ref() == Some(&data),
                "{len} level {level} prefixed"
            );
            let own = alt_zstd::decode_with_prefix(&frame, &prefix, len);
            assert!(own.as_ref() == Ok(&data), "{len} level {level} prefixed");
            cases += 2;
        }
        for level in [1, 3, 19] {
            let frame = reference::encode(&data, level);
            assert!(
                alt_zstd::decode_all(&frame).as_ref() == Ok(&data),
                "{len} level {level}"
            );
            let frame = reference::encode_with_prefix(&data, &prefix, level);
            let own = alt_zstd::decode_with_prefix(&frame, &prefix, len);
            assert!(own.as_ref() == Ok(&data), "{len} level {level} prefixed");
            cases += 2;
        }
    }
    assert_eq!(cases, 313 * 10);
}
