//! Frozen behaviour of a type: bytes serde wrote, and inputs with the
//! verdict serde gave them.
//!
//! One line per case. `=` is followed by the compact text of a value, which
//! must be read and written back unchanged; `A` by a text that must be
//! accepted and `R` by one that must be rejected.

use alt_json::{FromJson, ToJson};

use crate::drive::{duplicate_key, edit};
use crate::rng::Rng;
use crate::support::{Comparable, Tally, input, output};

/// Builds the fixture of `T` from `samples` values, checking alt-json
/// against serde on every line it records.
pub fn record<T: Comparable>(seed: u64, samples: u64, make: impl Fn(&mut Rng) -> T) -> String {
    let mut rng = Rng::new(seed);
    let mut tally = Tally::default();
    let mut lines = Vec::new();
    let mut verdict = |text: Vec<u8>, lines: &mut Vec<String>| {
        input::<T>(&text, &mut tally);
        let mark = if serde_json::from_slice::<T>(&text).is_ok() {
            'A'
        } else {
            'R'
        };
        lines.push(format!("{mark} {}", String::from_utf8(text).unwrap()));
    };
    for _ in 0..samples {
        let value = make(&mut rng);
        let compact = output(&value).expect("fixture values must be writable");
        lines.push(format!("= {}", String::from_utf8(compact.clone()).unwrap()));
        let tree = alt_json::parse(&compact).unwrap();
        verdict(alt_json::to_vec(&tree).unwrap(), &mut lines);
        for _ in 0..4 {
            let mut edited = tree.clone();
            for _ in 0..1 + rng.below(2) {
                edit(&mut rng, &mut edited);
            }
            verdict(alt_json::to_vec(&edited).unwrap(), &mut lines);
        }
        if let Some(repeated) = duplicate_key(&mut rng, &compact) {
            verdict(repeated, &mut lines);
        }
    }
    lines.sort();
    lines.dedup();
    lines.join("\n") + "\n"
}

/// What a fixture held.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub written: usize,
    pub accepted: usize,
    pub rejected: usize,
}

/// Holds `T` to a fixture made by [`record`].
pub fn verify<T: ToJson + FromJson>(fixture: &str) -> Counts {
    let mut counts = Counts::default();
    for line in fixture.lines() {
        let (mark, text) = line.split_at(2);
        let read = alt_json::from_str::<T>(text);
        match mark {
            "= " => {
                let value = read.unwrap_or_else(|e| panic!("{text}: {e}"));
                assert_eq!(alt_json::to_string(&value).unwrap(), text);
                counts.written += 1;
            }
            "A " => {
                assert!(read.is_ok(), "must be accepted: {text}");
                counts.accepted += 1;
            }
            "R " => {
                assert!(read.is_err(), "must be rejected: {text}");
                counts.rejected += 1;
            }
            _ => panic!("malformed fixture line: {line}"),
        }
    }
    counts
}
