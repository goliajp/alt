//! Number parsing.
//!
//! Integers that fit `u64` (or `i64` when negative) stay integers. Everything
//! else becomes an `f64` computed from at most the first nineteen or twenty
//! significant digits with one multiplication or division by a power of ten,
//! so the result can differ from the nearest float in the last place. Stored
//! documents were read this way, and keeping the conversion keeps every
//! number, and every digest computed over re-encoded documents, unchanged.

use super::Parser;
use crate::error::ErrorCode;
use crate::{Number, Result, Value};

/// A number as the grammar classifies it.
#[derive(Clone, Copy)]
pub(crate) enum Parsed {
    U64(u64),
    /// always negative
    I64(i64),
    F64(f64),
}

impl Parsed {
    pub(crate) fn into_value(self) -> Value {
        match self {
            Parsed::U64(n) => Value::Number(n.into()),
            Parsed::I64(n) => Value::Number(n.into()),
            Parsed::F64(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
        }
    }
}

fn push_digit(significand: u64, digit: u8) -> Option<u64> {
    significand
        .checked_mul(10)?
        .checked_add(u64::from(digit - b'0'))
}

impl Parser<'_> {
    /// Parses a number whose minus sign, if any, has been consumed.
    pub(crate) fn parse_number(&mut self, positive: bool) -> Result<Parsed> {
        let Some(first) = self.next() else {
            return Err(self.error(ErrorCode::EofWhileParsingValue));
        };
        match first {
            b'0' => match self.peek_or_null() {
                b'0'..=b'9' => Err(self.peek_error(ErrorCode::InvalidNumber)),
                _ => self.parse_after_integer(positive, 0),
            },
            b'1'..=b'9' => {
                let mut significand = u64::from(first - b'0');
                while let digit @ b'0'..=b'9' = self.peek_or_null() {
                    match push_digit(significand, digit) {
                        Some(grown) => significand = grown,
                        None => {
                            return self
                                .parse_long_integer(positive, significand)
                                .map(Parsed::F64);
                        }
                    }
                    self.eat();
                }
                self.parse_after_integer(positive, significand)
            }
            _ => Err(self.error(ErrorCode::InvalidNumber)),
        }
    }

    fn parse_after_integer(&mut self, positive: bool, significand: u64) -> Result<Parsed> {
        match self.peek_or_null() {
            b'.' => self
                .parse_decimal(positive, significand, 0)
                .map(Parsed::F64),
            b'e' | b'E' => self
                .parse_exponent(positive, significand, 0)
                .map(Parsed::F64),
            _ if positive => Ok(Parsed::U64(significand)),
            _ => {
                let negated = (significand as i64).wrapping_neg();
                // zero and magnitudes past i64::MIN have no i64 form
                if negated >= 0 {
                    Ok(Parsed::F64(-(significand as f64)))
                } else {
                    Ok(Parsed::I64(negated))
                }
            }
        }
    }

    /// The integer part no longer fits `u64`: the remaining integer digits
    /// only raise the exponent.
    #[cold]
    fn parse_long_integer(&mut self, positive: bool, significand: u64) -> Result<f64> {
        let mut exponent = 0;
        loop {
            match self.peek_or_null() {
                b'0'..=b'9' => {
                    self.eat();
                    exponent += 1;
                }
                b'.' => return self.parse_decimal(positive, significand, exponent),
                b'e' | b'E' => return self.parse_exponent(positive, significand, exponent),
                _ => return self.f64_from_parts(positive, significand, exponent),
            }
        }
    }

    fn parse_decimal(
        &mut self,
        positive: bool,
        mut significand: u64,
        exponent_before_point: i32,
    ) -> Result<f64> {
        self.eat();
        let mut exponent_after_point = 0;
        let mut dropped_digits = false;
        while let digit @ b'0'..=b'9' = self.peek_or_null() {
            let Some(grown) = push_digit(significand, digit) else {
                // digits past the capacity of u64 are dropped
                while let b'0'..=b'9' = self.peek_or_null() {
                    self.eat();
                }
                dropped_digits = true;
                break;
            };
            significand = grown;
            self.eat();
            exponent_after_point -= 1;
        }
        let exponent = exponent_before_point + exponent_after_point;
        if exponent_after_point == 0 && !dropped_digits {
            return Err(match self.peek() {
                Some(_) => self.peek_error(ErrorCode::InvalidNumber),
                None => self.peek_error(ErrorCode::EofWhileParsingValue),
            });
        }
        match self.peek_or_null() {
            b'e' | b'E' => self.parse_exponent(positive, significand, exponent),
            _ => self.f64_from_parts(positive, significand, exponent),
        }
    }

    fn parse_exponent(
        &mut self,
        positive: bool,
        significand: u64,
        starting_exponent: i32,
    ) -> Result<f64> {
        self.eat();
        let positive_exponent = match self.peek_or_null() {
            b'+' => {
                self.eat();
                true
            }
            b'-' => {
                self.eat();
                false
            }
            _ => true,
        };
        let mut exponent = match self.next() {
            Some(digit @ b'0'..=b'9') => i32::from(digit - b'0'),
            Some(_) => return Err(self.error(ErrorCode::InvalidNumber)),
            None => return Err(self.error(ErrorCode::EofWhileParsingValue)),
        };
        while let digit @ b'0'..=b'9' = self.peek_or_null() {
            self.eat();
            let grown = exponent
                .checked_mul(10)
                .and_then(|e| e.checked_add(i32::from(digit - b'0')));
            match grown {
                Some(grown) => exponent = grown,
                None => {
                    return self.exponent_overflow(positive, significand == 0, positive_exponent);
                }
            }
        }
        let exponent = if positive_exponent {
            starting_exponent.saturating_add(exponent)
        } else {
            starting_exponent.saturating_sub(exponent)
        };
        self.f64_from_parts(positive, significand, exponent)
    }

    /// The exponent does not fit `i32`: the value is zero, or too large.
    #[cold]
    fn exponent_overflow(
        &mut self,
        positive: bool,
        zero_significand: bool,
        positive_exponent: bool,
    ) -> Result<f64> {
        if !zero_significand && positive_exponent {
            return Err(self.error(ErrorCode::NumberOutOfRange));
        }
        while let b'0'..=b'9' = self.peek_or_null() {
            self.eat();
        }
        Ok(if positive { 0.0 } else { -0.0 })
    }

    fn f64_from_parts(
        &mut self,
        positive: bool,
        significand: u64,
        mut exponent: i32,
    ) -> Result<f64> {
        let mut f = significand as f64;
        loop {
            match POW10.get(exponent.unsigned_abs() as usize) {
                Some(&pow) => {
                    if exponent >= 0 {
                        f *= pow;
                        if f.is_infinite() {
                            return Err(self.error(ErrorCode::NumberOutOfRange));
                        }
                    } else {
                        f /= pow;
                    }
                    break;
                }
                None => {
                    if f == 0.0 {
                        break;
                    }
                    if exponent >= 0 {
                        return Err(self.error(ErrorCode::NumberOutOfRange));
                    }
                    f /= 1e308;
                    exponent += 308;
                }
            }
        }
        Ok(if positive { f } else { -f })
    }
}

static POW10: [f64; 309] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22, 1e23, 1e24, 1e25, 1e26, 1e27, 1e28, 1e29, 1e30, 1e31, 1e32,
    1e33, 1e34, 1e35, 1e36, 1e37, 1e38, 1e39, 1e40, 1e41, 1e42, 1e43, 1e44, 1e45, 1e46, 1e47, 1e48,
    1e49, 1e50, 1e51, 1e52, 1e53, 1e54, 1e55, 1e56, 1e57, 1e58, 1e59, 1e60, 1e61, 1e62, 1e63, 1e64,
    1e65, 1e66, 1e67, 1e68, 1e69, 1e70, 1e71, 1e72, 1e73, 1e74, 1e75, 1e76, 1e77, 1e78, 1e79, 1e80,
    1e81, 1e82, 1e83, 1e84, 1e85, 1e86, 1e87, 1e88, 1e89, 1e90, 1e91, 1e92, 1e93, 1e94, 1e95, 1e96,
    1e97, 1e98, 1e99, 1e100, 1e101, 1e102, 1e103, 1e104, 1e105, 1e106, 1e107, 1e108, 1e109, 1e110,
    1e111, 1e112, 1e113, 1e114, 1e115, 1e116, 1e117, 1e118, 1e119, 1e120, 1e121, 1e122, 1e123,
    1e124, 1e125, 1e126, 1e127, 1e128, 1e129, 1e130, 1e131, 1e132, 1e133, 1e134, 1e135, 1e136,
    1e137, 1e138, 1e139, 1e140, 1e141, 1e142, 1e143, 1e144, 1e145, 1e146, 1e147, 1e148, 1e149,
    1e150, 1e151, 1e152, 1e153, 1e154, 1e155, 1e156, 1e157, 1e158, 1e159, 1e160, 1e161, 1e162,
    1e163, 1e164, 1e165, 1e166, 1e167, 1e168, 1e169, 1e170, 1e171, 1e172, 1e173, 1e174, 1e175,
    1e176, 1e177, 1e178, 1e179, 1e180, 1e181, 1e182, 1e183, 1e184, 1e185, 1e186, 1e187, 1e188,
    1e189, 1e190, 1e191, 1e192, 1e193, 1e194, 1e195, 1e196, 1e197, 1e198, 1e199, 1e200, 1e201,
    1e202, 1e203, 1e204, 1e205, 1e206, 1e207, 1e208, 1e209, 1e210, 1e211, 1e212, 1e213, 1e214,
    1e215, 1e216, 1e217, 1e218, 1e219, 1e220, 1e221, 1e222, 1e223, 1e224, 1e225, 1e226, 1e227,
    1e228, 1e229, 1e230, 1e231, 1e232, 1e233, 1e234, 1e235, 1e236, 1e237, 1e238, 1e239, 1e240,
    1e241, 1e242, 1e243, 1e244, 1e245, 1e246, 1e247, 1e248, 1e249, 1e250, 1e251, 1e252, 1e253,
    1e254, 1e255, 1e256, 1e257, 1e258, 1e259, 1e260, 1e261, 1e262, 1e263, 1e264, 1e265, 1e266,
    1e267, 1e268, 1e269, 1e270, 1e271, 1e272, 1e273, 1e274, 1e275, 1e276, 1e277, 1e278, 1e279,
    1e280, 1e281, 1e282, 1e283, 1e284, 1e285, 1e286, 1e287, 1e288, 1e289, 1e290, 1e291, 1e292,
    1e293, 1e294, 1e295, 1e296, 1e297, 1e298, 1e299, 1e300, 1e301, 1e302, 1e303, 1e304, 1e305,
    1e306, 1e307, 1e308,
];
