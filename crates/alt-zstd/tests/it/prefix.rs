//! Frames that refer to a prefix: those of the reference implementation
//! are decoded from the pack, our own are decoded again and held to their
//! recorded digests.

use std::collections::HashMap;

use alt_zstd::{decode_all, decode_with_prefix, encode_all, encode_with_prefix};

use crate::inputs::{far_reach, mutate, prefixed, records, text};
use crate::{digests, pack};

/// records in `frames/prefixed.pack`
const PREFIXED_FRAMES: usize = 60;

#[test]
fn reference_prefixed_frames_decode() {
    let mut inputs: HashMap<_, _> = prefixed()
        .into_iter()
        .map(|(name, prefix, data)| (name, (prefix, data)))
        .collect();
    inputs.insert("far reach", far_reach());
    let records = pack::parse(pack::PREFIXED);
    for (key, frame) in &records {
        // the last part is a level, or "patch" for a frame made with a
        // wide window and long-distance matching
        let (name, _how) = key.rsplit_once('/').unwrap();
        let (prefix, data) = &inputs[name];
        let plain = decode_with_prefix(frame, prefix, data.len());
        assert!(plain.as_ref() == Ok(data), "{key}");
    }
    assert_eq!(records.len(), PREFIXED_FRAMES);
}

#[test]
fn own_prefixed_frames_round_trip_and_keep_their_digests() {
    let mut count = 0;
    for (name, prefix, data) in prefixed() {
        for level in [1, 3] {
            let frame = encode_with_prefix(&data, &prefix, level);
            let plain = decode_with_prefix(&frame, &prefix, data.len());
            assert!(plain.as_ref() == Ok(&data), "{name} level {level}");
            digests::expect(&format!("p/{name}/{level}"), &frame);
            count += 1;
        }
    }
    assert_eq!(count, 28);
}

#[test]
fn similar_data_costs_little() {
    let mut compared = 0;
    for (name, prefix, data) in prefixed() {
        if !name.starts_with("similar") && !name.starts_with("large") && name != "identical" {
            continue;
        }
        for level in [1, 3] {
            let frame = encode_with_prefix(&data, &prefix, level);
            let alone = encode_all(&data, level);
            println!(
                "{name} level {level}: {} -> {} with prefix, {} alone",
                data.len(),
                frame.len(),
                alone.len()
            );
            assert!(frame.len() * 4 < alone.len(), "{name} level {level}");
            compared += 1;
        }
    }
    assert_eq!(compared, 8);
}

#[test]
fn wrong_prefix_does_not_decode_to_the_data() {
    let prefix = text(60_000, 48);
    let data = mutate(&prefix, 6, 40);
    let frame = encode_with_prefix(&data, &prefix, 3);
    let other = text(60_000, 49);
    assert!(decode_with_prefix(&frame, &other, data.len()).as_ref() != Ok(&data));
    // too short a prefix leaves offsets pointing before the start of history
    assert!(decode_with_prefix(&frame, &prefix[..100], data.len()).is_err());
    assert!(decode_all(&frame).is_err());
}

/// A prefix of tens of megabytes: the declared window must stay within
/// what decoders accept by default, and data taken from the far start of
/// the prefix must still be found.
#[test]
fn far_reach_into_a_large_prefix() {
    let (prefix, data) = far_reach();
    for level in [1, 3] {
        let frame = encode_with_prefix(&data, &prefix, level);
        println!("level {level}: {} -> {}", data.len(), frame.len());
        assert!(
            frame.len() * 10 < data.len(),
            "level {level}: {}",
            frame.len()
        );
        // window descriptor right after the frame header descriptor
        assert!(10 + (frame[5] >> 3) <= 27);
        let plain = decode_with_prefix(&frame, &prefix, data.len());
        assert!(plain.as_ref() == Ok(&data), "level {level}");
        digests::expect(&format!("far/{level}"), &frame);
    }
}

/// Beyond 128 MiB of prefix the window stops growing, and data from the
/// reachable end of the prefix must still be found. No digest is recorded
/// for these frames; the window they declare is checked instead.
#[test]
#[ignore = "builds a 150 MiB prefix and holds about 400 MiB"]
fn prefix_larger_than_the_default_window() {
    let prefix = [records(75 << 20, 53), text(75 << 20, 54)].concat();
    let near = mutate(&prefix[(140 << 20)..(141 << 20)], 9, 100);
    let far = mutate(&prefix[(1 << 20)..(2 << 20)], 10, 100);
    for (name, data, found) in [("near", near, true), ("far", far, false)] {
        let frame = encode_with_prefix(&data, &prefix, 3);
        println!("{name}: {} -> {}", data.len(), frame.len());
        assert_eq!(10 + (frame[5] >> 3), 27, "{name}");
        assert_eq!(frame.len() * 10 < data.len(), found, "{name}");
        let plain = decode_with_prefix(&frame, &prefix, data.len());
        assert!(plain.as_ref() == Ok(&data), "{name}");
    }
}
