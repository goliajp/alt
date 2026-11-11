//! Strict RFC 4648 base64.
//!
//! Two engines are provided: [`STANDARD`] (the `+/` alphabet with `=`
//! padding) and [`URL_SAFE_NO_PAD`] (the `-_` alphabet without padding).
//! Decoding accepts exactly the strings the matching encoder produces:
//! no whitespace, no missing or surplus padding, and no set bits in the
//! unused tail of the last symbol.
#![forbid(unsafe_code)]

use std::fmt;

const INVALID: u8 = 0xff;

/// A base64 alphabet together with its padding rule.
pub struct Engine {
    symbols: &'static [u8; 64],
    values: [u8; 256],
    padded: bool,
}

/// The standard alphabet (`A-Z a-z 0-9 + /`) with mandatory `=` padding.
pub const STANDARD: Engine = Engine::new(
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/",
    true,
);

/// The URL-safe alphabet (`A-Z a-z 0-9 - _`) with padding forbidden.
pub const URL_SAFE_NO_PAD: Engine = Engine::new(
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_",
    false,
);

/// Why an input is not the canonical encoding of any byte string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// A byte outside the alphabet, including `=` anywhere but the last
    /// two positions of a padded encoding.
    InvalidSymbol,
    /// The number of symbols cannot encode a whole number of bytes.
    InvalidLength,
    /// The engine requires padding and the input is not a whole number of
    /// four-byte groups.
    InvalidPadding,
    /// The last symbol carries bits that belong to no output byte.
    TrailingBits,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidSymbol => "byte outside the base64 alphabet",
            Self::InvalidLength => "base64 input has an impossible length",
            Self::InvalidPadding => "invalid base64 padding",
            Self::TrailingBits => "non-canonical trailing bits in base64 input",
        })
    }
}

impl std::error::Error for DecodeError {}

impl Engine {
    const fn new(symbols: &'static [u8; 64], padded: bool) -> Self {
        let mut values = [INVALID; 256];
        let mut i = 0;
        while i < 64 {
            values[symbols[i] as usize] = i as u8;
            i += 1;
        }
        Self {
            symbols,
            values,
            padded,
        }
    }

    /// Encodes `input` as a base64 string.
    pub fn encode(&self, input: impl AsRef<[u8]>) -> String {
        let input = input.as_ref();
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        let (chunks, tail) = input.as_chunks::<3>();
        for chunk in chunks {
            let n = u32::from(chunk[0]) << 16 | u32::from(chunk[1]) << 8 | u32::from(chunk[2]);
            out.extend([18, 12, 6, 0].map(|shift| self.symbol(n >> shift)));
        }
        if !tail.is_empty() {
            let n = u32::from(tail[0]) << 16 | u32::from(*tail.get(1).unwrap_or(&0)) << 8;
            let symbols = tail.len() + 1;
            out.extend([18, 12, 6][..symbols].iter().map(|&s| self.symbol(n >> s)));
            if self.padded {
                out.extend(std::iter::repeat_n('=', 4 - symbols));
            }
        }
        out
    }

    /// Decodes `input`, rejecting anything [`Engine::encode`] would not
    /// have produced.
    pub fn decode(&self, input: impl AsRef<[u8]>) -> Result<Vec<u8>, DecodeError> {
        let symbols = self.strip_padding(input.as_ref())?;
        let (quads, tail) = symbols.as_chunks::<4>();
        if tail.len() == 1 {
            return Err(DecodeError::InvalidLength);
        }
        let mut out = Vec::with_capacity(quads.len() * 3 + 2);
        for quad in quads {
            let n = self.sextets(quad)?;
            out.extend_from_slice(&n.to_be_bytes()[1..]);
        }
        if !tail.is_empty() {
            let n = self.sextets(tail)? << (6 * (4 - tail.len()));
            let unused_bits = if tail.len() == 2 { 16 } else { 8 };
            if n & ((1 << unused_bits) - 1) != 0 {
                return Err(DecodeError::TrailingBits);
            }
            out.extend_from_slice(&n.to_be_bytes()[1..tail.len()]);
        }
        Ok(out)
    }

    fn symbol(&self, bits: u32) -> char {
        char::from(self.symbols[(bits & 63) as usize])
    }

    /// Returns the symbols with the padding removed. A `=` left in the
    /// result is rejected later as a byte outside the alphabet.
    fn strip_padding<'a>(&self, input: &'a [u8]) -> Result<&'a [u8], DecodeError> {
        if !self.padded {
            return Ok(input);
        }
        if !input.len().is_multiple_of(4) {
            return Err(DecodeError::InvalidPadding);
        }
        let pads = input
            .iter()
            .rev()
            .take(2)
            .take_while(|&&b| b == b'=')
            .count();
        Ok(&input[..input.len() - pads])
    }

    fn sextets(&self, symbols: &[u8]) -> Result<u32, DecodeError> {
        symbols
            .iter()
            .try_fold(0, |n, &symbol| match self.values[symbol as usize] {
                INVALID => Err(DecodeError::InvalidSymbol),
                value => Ok(n << 6 | u32::from(value)),
            })
    }
}
