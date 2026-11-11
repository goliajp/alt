use std::fmt;
use std::hash::{Hash, Hasher};

use crate::parse::Parsed;

/// A JSON number: a non-negative integer, a negative integer or a finite
/// float. Numbers of different kinds are never equal, so `1` differs from
/// `1.0`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Number {
    n: N,
}

#[derive(Clone, Copy)]
pub(crate) enum N {
    PosInt(u64),
    /// always less than zero
    NegInt(i64),
    /// always finite
    Float(f64),
}

impl PartialEq for N {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (N::PosInt(a), N::PosInt(b)) => a == b,
            (N::NegInt(a), N::NegInt(b)) => a == b,
            (N::Float(a), N::Float(b)) => a == b,
            _ => false,
        }
    }
}

// floats are always finite, so equality is reflexive
impl Eq for N {}

impl Hash for N {
    fn hash<H: Hasher>(&self, h: &mut H) {
        match *self {
            N::PosInt(i) => i.hash(h),
            N::NegInt(i) => i.hash(h),
            N::Float(f) => {
                // +0.0 and -0.0 compare equal, so they hash alike
                let f = if f == 0.0 { 0.0 } else { f };
                f.to_bits().hash(h);
            }
        }
    }
}

impl Number {
    pub(crate) fn kind(&self) -> N {
        self.n
    }

    pub(crate) fn from_parsed(parsed: Parsed) -> Number {
        let n = match parsed {
            Parsed::U64(n) => N::PosInt(n),
            Parsed::I64(n) => N::NegInt(n),
            Parsed::F64(n) => N::Float(n),
        };
        Number { n }
    }

    /// True for an integer between `i64::MIN` and `i64::MAX`.
    pub fn is_i64(&self) -> bool {
        match self.n {
            N::PosInt(v) => v <= i64::MAX as u64,
            N::NegInt(_) => true,
            N::Float(_) => false,
        }
    }

    /// True for an integer between zero and `u64::MAX`.
    pub fn is_u64(&self) -> bool {
        matches!(self.n, N::PosInt(_))
    }

    /// True for a number stored as a float, which is every number written
    /// with a fraction or exponent and every integer outside the `i64` and
    /// `u64` ranges.
    pub fn is_f64(&self) -> bool {
        matches!(self.n, N::Float(_))
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self.n {
            N::PosInt(n) => i64::try_from(n).ok(),
            N::NegInt(n) => Some(n),
            N::Float(_) => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self.n {
            N::PosInt(n) => Some(n),
            N::NegInt(_) | N::Float(_) => None,
        }
    }

    /// The number as a float; integers are converted, rounding if needed.
    pub fn as_f64(&self) -> Option<f64> {
        Some(match self.n {
            N::PosInt(n) => n as f64,
            N::NegInt(n) => n as f64,
            N::Float(n) => n,
        })
    }

    /// `None` for infinities and NaN, which JSON cannot represent.
    pub fn from_f64(f: f64) -> Option<Number> {
        f.is_finite().then_some(Number { n: N::Float(f) })
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = Vec::with_capacity(24);
        crate::write::write_number(self, &mut text);
        // number text is ascii
        f.write_str(std::str::from_utf8(&text).map_err(|_| fmt::Error)?)
    }
}

impl fmt::Debug for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Number({self})")
    }
}

macro_rules! from_unsigned {
    ($($ty:ty)*) => {$(
        impl From<$ty> for Number {
            fn from(u: $ty) -> Self {
                Number { n: N::PosInt(u as u64) }
            }
        }
    )*};
}

macro_rules! from_signed {
    ($($ty:ty)*) => {$(
        impl From<$ty> for Number {
            fn from(i: $ty) -> Self {
                let n = if i < 0 {
                    N::NegInt(i as i64)
                } else {
                    N::PosInt(i as u64)
                };
                Number { n }
            }
        }
    )*};
}

from_unsigned!(u8 u16 u32 u64 usize);
from_signed!(i8 i16 i32 i64 isize);
