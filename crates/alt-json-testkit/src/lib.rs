//! Differential testing of alt-json against serde and serde_json.
//!
//! [`drive::exercise`] writes random values of a type through both
//! libraries and reads back their text, damaged and edited in many ways,
//! requiring the same bytes, the same verdicts and the same values.
//! [`fixtures`] freezes what serde produced and accepted, so a type can be
//! held to it after its serde implementation is gone.

pub mod check;
pub mod drive;
pub mod fixtures;
pub mod generate;
pub mod rng;
pub mod support;
