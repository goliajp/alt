//! Comparison of alt-json with serde for types that have only one
//! direction: responses that are only written, inputs that are only read.

use crate::check::same;
use crate::support::{Tally, category, show};

/// For a type that is only ever written: the compact and indented text
/// and the tree form of `value` must match. Returns the compact text.
pub fn written<T: alt_json::ToJson + serde::Serialize>(value: &T) -> Vec<u8> {
    let compact = alt_json::to_vec(value).unwrap();
    assert_eq!(
        show(&compact),
        show(&serde_json::to_vec(value).unwrap()),
        "compact output"
    );
    assert_eq!(compact, serde_json::to_vec(value).unwrap());
    assert_eq!(
        alt_json::to_vec_pretty(value).unwrap(),
        serde_json::to_vec_pretty(value).unwrap(),
        "indented output of {}",
        show(&compact)
    );
    assert!(
        same(
            &alt_json::to_value(value).unwrap(),
            &serde_json::to_value(value).unwrap()
        ),
        "tree of {}",
        show(&compact)
    );
    compact
}

/// Reads `input` as a type that is only ever read: same verdict, same
/// error category and position, and the same `view` of the value. Then the
/// same from the tree form of the input.
pub fn read<T>(input: &[u8], view: fn(&T) -> String, tally: &mut Tally)
where
    T: alt_json::FromJson + serde::de::DeserializeOwned,
{
    tally.inputs += 1;
    match (
        alt_json::from_slice::<T>(input),
        serde_json::from_slice::<T>(input),
    ) {
        (Ok(ours), Ok(theirs)) => {
            assert_eq!(view(&ours), view(&theirs), "value from {}", show(input));
            tally.accepted += 1;
        }
        (Err(ours), Err(theirs)) => assert_eq!(
            (ours.classify(), ours.line(), ours.column()),
            (category(&theirs), theirs.line(), theirs.column()),
            "error from {}: {ours} vs {theirs}",
            show(input)
        ),
        (ours, theirs) => panic!(
            "verdict differs for {}: {:?} vs {:?}",
            show(input),
            ours.map(|_| ()),
            theirs.map(|_| ())
        ),
    }
    let (Ok(ours), Ok(theirs)) = (
        alt_json::parse(input),
        serde_json::from_slice::<serde_json::Value>(input),
    ) else {
        return;
    };
    tally.tree_inputs += 1;
    match (
        alt_json::from_value::<T>(ours),
        serde_json::from_value::<T>(theirs),
    ) {
        (Ok(ours), Ok(theirs)) => {
            assert_eq!(
                view(&ours),
                view(&theirs),
                "value from tree {}",
                show(input)
            );
            tally.tree_accepted += 1;
        }
        (Err(ours), Err(theirs)) => assert_eq!(
            (ours.classify(), ours.line()),
            (category(&theirs), theirs.line()),
            "tree error from {}: {ours} vs {theirs}",
            show(input)
        ),
        (ours, theirs) => panic!(
            "tree verdict differs for {}: {:?} vs {:?}",
            show(input),
            ours.map(|_| ()),
            theirs.map(|_| ())
        ),
    }
}
