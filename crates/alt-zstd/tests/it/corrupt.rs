//! Damaged input must be refused, never crash, hang or over-allocate, and
//! whatever is accepted from a frame with a content checksum must be the
//! original content.

use std::collections::HashMap;

use alt_zstd::{decode_all, decode_with_prefix};

use crate::inputs::{Rng, damage_sources, mutate, text};
use crate::{digests, pack};

/// records in `frames/damaged.pack`
const DAMAGED_FRAMES: usize = 271;

struct Source {
    name: String,
    frame: Vec<u8>,
    data: Vec<u8>,
    /// whether the frame carries a content checksum
    checked: bool,
}

/// frames from both encoders that between them use every block kind; the
/// reference ones carry a content checksum, ours do not
fn sources() -> Vec<Source> {
    let inputs: HashMap<_, _> = damage_sources().into_iter().collect();
    let mut sources = Vec::new();
    for (key, frame) in pack::parse(pack::CHECKED) {
        let Some(rest) = key.strip_prefix("source/") else {
            continue;
        };
        let (name, _level) = rest.rsplit_once('/').unwrap();
        sources.push(Source {
            name: format!("reference {rest}"),
            frame: frame.to_vec(),
            data: inputs[name].clone(),
            checked: true,
        });
    }
    assert_eq!(sources.len(), 10);
    for (name, data) in damage_sources() {
        sources.push(Source {
            name: format!("own {name}"),
            frame: alt_zstd::encode_all(&data, 3),
            data,
            checked: false,
        });
    }
    sources
}

/// Decodes damaged input and returns whether it was accepted. Accepting is
/// only right, for a frame with a checksum, if the content is unchanged.
fn survives(source: &Source, what: &str, damaged: &[u8]) -> bool {
    match decode_all(damaged) {
        Ok(out) => {
            assert!(
                !source.checked || out == source.data,
                "{what}: wrong content"
            );
            true
        }
        Err(_) => false,
    }
}

#[test]
fn truncation_is_an_error() {
    let mut checked = 0;
    for Source { name, frame, .. } in sources() {
        let step = (frame.len() / 600).max(1);
        let cuts = (0..frame.len()).step_by(step);
        for len in cuts.chain(frame.len() - 8..frame.len()) {
            assert!(decode_all(&frame[..len]).is_err(), "{name} cut at {len}");
            checked += 1;
        }
    }
    assert!(checked > 5000, "{checked}");
}

#[test]
fn bit_flips_never_pass_a_checksum() {
    let (mut flips, mut accepted, mut accepted_checked) = (0, 0, 0);
    for source in sources() {
        let step = (source.frame.len() / 400).max(1);
        for at in (0..source.frame.len()).step_by(step) {
            for bit in 0..8 {
                let mut damaged = source.frame.clone();
                damaged[at] ^= 1 << bit;
                let what = format!("{} byte {at} bit {bit}", source.name);
                if survives(&source, &what, &damaged) {
                    accepted += 1;
                    accepted_checked += source.checked as usize;
                }
                flips += 1;
            }
        }
    }
    println!("{flips} flips: {accepted} accepted, {accepted_checked} of them with a checksum");
    assert!(flips > 40_000, "{flips}");
    // with a checksum only flips of bits that carry no meaning get through
    assert!(accepted_checked < flips / 200, "{accepted_checked}");
}

#[test]
fn splices_and_overwrites_never_pass_a_checksum() {
    let mut rng = Rng::new(67);
    let mut tried = 0;
    for source in sources() {
        for round in 0..300 {
            let mut damaged = source.frame.clone();
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
            survives(&source, &format!("{} round {round}", source.name), &damaged);
            tried += 1;
        }
    }
    assert_eq!(tried, 16 * 300);
}

#[test]
fn noise_behind_a_valid_header_never_crashes() {
    let mut rng = Rng::new(68);
    let mut accepted = 0;
    for round in 0..20_000 {
        let mut input = vec![0x28, 0xB5, 0x2F, 0xFD];
        let len = 6 + rng.below(200);
        input.extend((0..len).map(|_| (rng.next() >> 56) as u8));
        // steer half the rounds into a compressed block right away
        if round % 2 == 0 {
            input[4] = 0x20;
            input[5] = 0xFF;
            let body = (len.saturating_sub(5)) as u32;
            input[6..9].copy_from_slice(&(body << 3 | 0b101).to_le_bytes()[..3]);
        }
        accepted += decode_all(&input).is_ok() as usize;
    }
    println!("{accepted} of 20000 accepted");
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

/// Damaged frames that the reference implementation accepted when the pack
/// was made, each with the digest of what it decoded them to: we must
/// accept them too, with the same result.
#[test]
fn damage_the_reference_accepts_decodes_alike() {
    let records = pack::parse(pack::DAMAGED);
    for (key, record) in &records {
        let (digest, frame) = record.split_at(32);
        let out = decode_all(frame).unwrap_or_else(|error| panic!("{key}: {error}"));
        assert!(
            alt_hash::sha256(&out) == digest,
            "{key}: {}",
            digests::hex(digest)
        );
    }
    assert_eq!(records.len(), DAMAGED_FRAMES);
}
