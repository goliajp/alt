//! Differential testing of alt-json against serde and serde_json.
//!
//! [`drive::exercise`] writes random values of a type through both
//! libraries and reads back their text, damaged and edited in many ways,
//! requiring the same bytes, the same verdicts and the same values.
//! [`fixtures`] freezes what serde produced and accepted, so a type can be
//! held to it after its serde implementation is gone. [`reads`] does the
//! same for what a type reads, with the value or the error message.

pub mod arbitrary;
pub mod check;
pub mod drive;
pub mod fixtures;
pub mod generate;
pub mod oneway;
pub mod reads;
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

/// For types private to a module: generates, inside that module, the
/// comparison with serde and the recording of one fixture per type under
/// `tests/fixtures/json/<prefix>-<Type>.txt` of the calling crate.
#[macro_export]
macro_rules! module_tests {
    ($prefix:literal, $seed:expr; $($ty:ident),* $(,)?) => {
        #[test]
        fn same_as_serde() {
            $crate::same_as_serde!($seed, 300; $($ty),*).report(concat!("module ", $prefix));
        }

        #[test]
        fn recorded_bytes_and_verdicts() {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");
            $(
                $crate::fixtures::check::<$ty>(
                    dir,
                    concat!($prefix, "-", stringify!($ty)),
                    $seed ^ (stringify!($ty).len() as u64) << 16,
                    15,
                );
            )*
        }
    };
}

/// What [`module_tests`] becomes once the types no longer have serde
/// implementations: the stored fixtures alone.
#[macro_export]
macro_rules! module_fixture_tests {
    ($prefix:literal; $($ty:ident),* $(,)?) => {
        #[test]
        fn recorded_bytes_and_verdicts() {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");
            $(
                $crate::fixtures::verify_file::<$ty>(dir, concat!($prefix, "-", stringify!($ty)));
            )*
        }
    };
}

/// Holds what the listed types read to the stored fixtures
/// `tests/fixtures/json-read/<prefix>-<name>.txt` of the calling crate.
#[macro_export]
macro_rules! module_read_tests {
    ($prefix:literal; $($name:ident: $ty:ty),* $(,)?) => {
        #[test]
        fn recorded_reads() {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json-read");
            $(
                $crate::reads::verify_file::<$ty>(dir, concat!($prefix, "-", stringify!($name)));
            )*
        }
    };
}
