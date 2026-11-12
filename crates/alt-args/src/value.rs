//! Typed argument values.

use std::net::SocketAddr;
use std::path::PathBuf;

pub type Check = fn(&str) -> Result<(), String>;

fn check<T: Value>(text: &str) -> Result<(), String> {
    T::parse(text).map(drop)
}

/// A type an argument value can be read as.
pub trait Value: Sized {
    /// the only accepted spellings when not empty
    const POSSIBLE: &'static [&'static str] = &[];
    /// run on each value while parsing; `None` when `parse` cannot fail
    const CHECK: Option<Check> = Some(check::<Self>);

    fn parse(text: &str) -> Result<Self, String>;
}

impl Value for String {
    const CHECK: Option<Check> = None;

    fn parse(text: &str) -> Result<Self, String> {
        Ok(text.to_owned())
    }
}

impl Value for PathBuf {
    const CHECK: Option<Check> = None;

    fn parse(text: &str) -> Result<Self, String> {
        Ok(PathBuf::from(text))
    }
}

impl Value for bool {
    const POSSIBLE: &'static [&'static str] = &["true", "false"];
    const CHECK: Option<Check> = None;

    fn parse(text: &str) -> Result<Self, String> {
        Ok(text == "true")
    }
}

macro_rules! from_str {
    ($($type:ty),*) => {$(
        impl Value for $type {
            fn parse(text: &str) -> Result<Self, String> {
                text.parse().map_err(|error: <$type as std::str::FromStr>::Err| error.to_string())
            }
        }
    )*};
}

from_str!(
    u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, SocketAddr
);
