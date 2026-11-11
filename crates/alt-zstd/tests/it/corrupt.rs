//! Damaged input must be refused, never crash, hang or over-allocate. The
//! reference decoder is run on the same damage: whatever it accepts we must
//! accept with the same bytes, and the other way round.

use alt_zstd::{decode_all, decode_with_prefix};

use crate::inputs::{Rng, mutate, random, records, runs, text};
use crate::reference;

/// small frames from both encoders that between them use every block kind
fn frames() -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let inputs = [
        ("text", text(6000, 61)),
        ("records", records(5000, 62)),
        ("runs", runs(40_000, 63)),
        ("random", random(700, 64)),
        ("multi-block", text(140_000, 65)),
        ("stitched", crate::inputs::stitched(66)),
    ];
    let mut frames = Vec::new();
    for (name, data) in inputs {
        for level in [1, 19] {
            let frame = reference::encode_checked(&data, level);
            frames.push((format!("reference {name} {level}"), frame, data.clone()));
        }
        let frame = alt_zstd::encode_all(&data, 3);
        frames.push((format!("own {name}"), frame, data.clone()));
    }
    frames
}

enum Verdict {
    Accepted,
    Refused,
    /// refused by us alone
    Stricter,
}

/// Decodes with both implementations. What we accept the reference must
/// accept with the same bytes, except that it refuses windows above 128 MiB
/// and we have no such limit. We may refuse more: we insist that every
/// Huffman stream ends exactly where its last symbol does.
fn verdict(what: &str, input: &[u8]) -> Verdict {
    match (decode_all(input), reference::decode(input)) {
        (Ok(ours), Ok(theirs)) => {
            assert!(ours == theirs, "{what}: outputs differ");
            Verdict::Accepted
        }
        (Err(_), Err(_)) => Verdict::Refused,
        (Ok(_), Err(error)) => {
            assert!(
                error.to_string().contains("too much memory"),
                "{what}: only we accept ({error})"
            );
            Verdict::Accepted
        }
        (Err(_), Ok(_)) => Verdict::Stricter,
    }
}

#[test]
fn truncation_is_an_error() {
    let mut checked = 0;
    for (name, frame, _) in frames() {
        let step = (frame.len() / 600).max(1);
        for len in (0..frame.len())
            .step_by(step)
            .chain(frame.len() - 8..frame.len())
        {
            assert!(decode_all(&frame[..len]).is_err(), "{name} cut at {len}");
            checked += 1;
        }
    }
    assert!(checked > 5000, "{checked}");
}

#[test]
fn bit_flips_get_the_reference_verdict() {
    let (mut flips, mut accepted, mut stricter) = (0, 0, 0);
    for (name, frame, data) in frames() {
        let step = (frame.len() / 400).max(1);
        for at in (0..frame.len()).step_by(step) {
            for bit in 0..8 {
                let mut damaged = frame.clone();
                damaged[at] ^= 1 << bit;
                let what = format!("{name} byte {at} bit {bit}");
                match verdict(&what, &damaged) {
                    Verdict::Accepted => {
                        accepted += 1;
                        // a checksummed frame survives only flips that
                        // change nothing
                        if name.starts_with("reference") {
                            assert!(decode_all(&damaged).unwrap() == data, "{what}");
                        }
                    }
                    Verdict::Stricter => stricter += 1,
                    Verdict::Refused => {}
                }
                flips += 1;
            }
        }
    }
    println!("{flips} flips: {accepted} accepted, {stricter} refused by us alone");
    assert!(flips > 40_000, "{flips}");
    assert!(stricter < flips / 20, "{stricter}");
}

#[test]
fn spliced_and_overwritten_frames_get_the_reference_verdict() {
    let mut rng = Rng::new(67);
    let mut tried = 0;
    for (name, frame, _) in frames() {
        for round in 0..300 {
            let mut damaged = frame.clone();
            let at = rng.below(damaged.len());
            match round % 4 {
                0 => damaged[at] = (rng.next() >> 56) as u8,
                1 => {
                    let end = (at + 1 + rng.below(8)).min(damaged.len());
                    damaged.drain(at..end);
                }
                2 => damaged.insert(at, (rng.next() >> 56) as u8),
                _ => {
                    let end = (at + rng.below(16)).min(damaged.len());
                    damaged[at..end].fill(0xFF);
                }
            }
            verdict(&format!("{name} round {round}"), &damaged);
            tried += 1;
        }
    }
    assert_eq!(tried, 18 * 300);
}

#[test]
fn noise_behind_a_valid_header() {
    let mut rng = Rng::new(68);
    for round in 0..20_000 {
        let mut input = vec![0x28, 0xB5, 0x2F, 0xFD];
        let len = 6 + rng.below(200);
        input.extend((0..len).map(|_| (rng.next() >> 56) as u8));
        // steer most rounds into a compressed block right away
        if round % 2 == 0 {
            input[4] = 0x20;
            input[5] = 0xFF;
            let body = (len.saturating_sub(5)) as u32;
            input[6..9].copy_from_slice(&(body << 3 | 0b101).to_le_bytes()[..3]);
        }
        verdict(&format!("round {round}"), &input);
    }
}

#[test]
fn damaged_prefixed_frames_never_crash() {
    let prefix = text(30_000, 69);
    let data = mutate(&prefix, 8, 60);
    let frame = alt_zstd::encode_with_prefix(&data, &prefix, 3);
    let mut refused = 0;
    for at in 0..frame.len() {
        for bit in [0, 3, 7] {
            let mut damaged = frame.clone();
            damaged[at] ^= 1 << bit;
            match decode_with_prefix(&damaged, &prefix, data.len()) {
                Ok(out) => assert_eq!(out.len(), data.len()),
                Err(_) => refused += 1,
            }
            // the reference must not accept what we refuse to produce
            let theirs = reference::decode_with_prefix(&damaged, &prefix, data.len());
            if let Some(theirs) = theirs.filter(|out| out.len() == data.len()) {
                let ours = decode_with_prefix(&damaged, &prefix, data.len());
                assert!(ours.as_ref() == Ok(&theirs), "byte {at} bit {bit}");
            }
        }
        assert!(decode_with_prefix(&frame[..at], &prefix, data.len()).is_err());
    }
    println!(
        "{} bytes, {refused} of {} flips refused",
        frame.len(),
        3 * frame.len()
    );
    assert!(refused > frame.len() / 2);
}
