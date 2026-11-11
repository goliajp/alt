//! Differential testing of alt-json against serde and serde_json.
//!
//! [`drive::exercise`] writes random values of a type through both
//! libraries and reads back their text, damaged and edited in many ways,
//! requiring the same bytes, the same verdicts and the same values.
//! [`fixtures`] freezes what serde produced and accepted, so a type can be
//! held to it after its serde implementation is gone.

pub mod arbitrary;
pub mod check;
pub mod drive;
pub mod fixtures;
pub mod generate;
pub mod rng;
pub mod support;

/// Compares alt-json with serde on each listed type, with random values
/// produced from the type's own `Deserialize` implementation, and returns
/// the totals.
#[macro_export]
macro_rules! same_as_serde {
    ($seed:expr, $rounds:expr; $($ty:ty),* $(,)?) => {{
        let mut total = $crate::support::Tally::default();
        $(
            let seed = $seed ^ (stringify!($ty).len() as u64) << 8;
            let tally = $crate::drive::exercise_with::<$ty>(seed, $rounds, $crate::arbitrary::from_serde);
            tally.report(stringify!($ty));
            total.add(&tally);
        )*
        total
    }};
}
