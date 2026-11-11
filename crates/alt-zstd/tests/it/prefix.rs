use alt_zstd::{decode_with_prefix, encode_with_prefix};

use crate::inputs::{mutate, random, records, text};
use crate::reference;

/// (name, prefix, data)
fn cases() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let page = text(60_000, 41);
    let binary = records(400_000, 42);
    let big = [text(3 << 20, 43), records(3 << 20, 44)].concat();
    vec![
        ("similar text", page.clone(), mutate(&page, 1, 40)),
        ("similar binary", binary.clone(), mutate(&binary, 2, 300)),
        ("identical", page.clone(), page.clone()),
        ("unrelated", random(50_000, 45), text(80_000, 46)),
        ("empty prefix", Vec::new(), text(90_000, 47)),
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
            mutate(&big[(4 << 20)..(4 << 20) + 70_000], 3, 30),
        ),
        (
            "data much larger",
            page[..5000].to_vec(),
            mutate(&big[..2 << 20], 4, 500),
        ),
        ("large both", big.clone(), mutate(&big, 5, 2000)),
        (
            "match across the seam",
            page.clone(),
            [&page[59_000..], &page[..500]].concat(),
        ),
    ]
}

#[test]
fn reference_reads_our_prefixed_frames() {
    let mut count = 0;
    for (name, prefix, data) in cases() {
        for level in [1, 3] {
            let frame = encode_with_prefix(&data, &prefix, level);
            let plain = reference::decode_with_prefix(&frame, &prefix, data.len());
            assert!(
                plain.as_ref() == Some(&data),
                "{name} level {level}: reference"
            );
            let own = decode_with_prefix(&frame, &prefix, data.len());
            assert!(own.as_ref() == Ok(&data), "{name} level {level}: own");
            count += 1;
        }
    }
    assert_eq!(count, 28);
}

#[test]
fn we_read_reference_prefixed_frames() {
    let mut count = 0;
    for (name, prefix, data) in cases() {
        for level in [1, 3, 9, 19] {
            let frame = reference::encode_with_prefix(&data, &prefix, level);
            let own = decode_with_prefix(&frame, &prefix, data.len());
            assert!(own.as_ref() == Ok(&data), "{name} level {level}");
            count += 1;
        }
        // the patch-from configuration: wide window, long-distance matching
        let frame = reference::encode_patch(&data, &prefix, 3, 27);
        let own = decode_with_prefix(&frame, &prefix, data.len());
        assert!(own.as_ref() == Ok(&data), "{name} patch");
        count += 1;
    }
    assert_eq!(count, 70);
}

#[test]
fn similar_data_costs_little() {
    for (name, prefix, data) in cases() {
        if !name.starts_with("similar") && !name.starts_with("large") && name != "identical" {
            continue;
        }
        for level in [1, 3] {
            let frame = encode_with_prefix(&data, &prefix, level);
            let alone = alt_zstd::encode_all(&data, level);
            let theirs = reference::encode_with_prefix(&data, &prefix, level);
            println!(
                "{name} level {level}: {} -> {} with prefix, {} alone, reference {}",
                data.len(),
                frame.len(),
                alone.len(),
                theirs.len()
            );
            assert!(frame.len() * 4 < alone.len(), "{name} level {level}");
        }
    }
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
    assert!(alt_zstd::decode_all(&frame).is_err());
}

/// A prefix of tens of megabytes: the frame must stay readable by the
/// reference decoder with its default window limit, and data taken from the
/// far start of the prefix must still be found.
#[test]
fn far_reach_into_a_large_prefix() {
    let prefix = [records(20 << 20, 51), text(20 << 20, 52)].concat();
    let data = mutate(&prefix[1000..(1 << 20) + 1000], 7, 200);
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
        let plain = reference::decode_with_prefix(&frame, &prefix, data.len());
        assert!(plain.as_ref() == Some(&data), "level {level}: reference");
        let own = decode_with_prefix(&frame, &prefix, data.len());
        assert!(own.as_ref() == Ok(&data), "level {level}: own");
    }
    let frame = reference::encode_patch(&data, &prefix, 3, 27);
    assert!(decode_with_prefix(&frame, &prefix, data.len()).as_ref() == Ok(&data));
}

/// Beyond 128 MiB of prefix the window stops growing: the frame must still
/// be readable by the reference decoder with default parameters, and data
/// from the reachable end of the prefix must still be found.
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
        let plain = reference::decode_with_prefix(&frame, &prefix, data.len());
        assert!(plain.as_ref() == Some(&data), "{name}: reference");
        let own = decode_with_prefix(&frame, &prefix, data.len());
        assert!(own.as_ref() == Ok(&data), "{name}: own");
    }
}
