//! Differential tests: every case runs through this crate and through
//! serde_json, and the two must agree on output bytes, on whether an input
//! is accepted, on the parsed value and on the error text.

mod api;
mod derive;
mod float_format;
mod float_parse;
mod integers;
mod invalid;
mod macros;
mod strings;
mod syntax;
mod values;

use alt_json_testkit::{check, generate, rng};
