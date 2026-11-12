//! Binary encoding of parameters and decoding of result values.

/// a type's object identifier in `pg_type`
pub type Oid = u32;

pub mod oid {
    use super::Oid;
    pub const BOOL: Oid = 16;
    pub const BYTEA: Oid = 17;
    pub const CHAR: Oid = 18;
    pub const NAME: Oid = 19;
    pub const INT8: Oid = 20;
    pub const INT2: Oid = 21;
    pub const INT4: Oid = 23;
    pub const TEXT: Oid = 25;
    pub const OID: Oid = 26;
    pub const JSON: Oid = 114;
    pub const FLOAT4: Oid = 700;
    pub const FLOAT8: Oid = 701;
    pub const UNKNOWN: Oid = 705;
    pub const BPCHAR: Oid = 1042;
    pub const VARCHAR: Oid = 1043;
    pub const JSONB: Oid = 3802;
}

/// the element type of a one-dimensional array type
pub(crate) fn element(array: Oid) -> Option<Oid> {
    Some(match array {
        1000 => oid::BOOL,
        1001 => oid::BYTEA,
        1002 => oid::CHAR,
        1003 => oid::NAME,
        1005 => oid::INT2,
        1007 => oid::INT4,
        1009 => oid::TEXT,
        1014 => oid::BPCHAR,
        1015 => oid::VARCHAR,
        1016 => oid::INT8,
        1021 => oid::FLOAT4,
        1022 => oid::FLOAT8,
        1028 => oid::OID,
        199 => oid::JSON,
        3807 => oid::JSONB,
        _ => return None,
    })
}

pub(crate) fn is_text(ty: Oid) -> bool {
    matches!(
        ty,
        oid::TEXT | oid::VARCHAR | oid::BPCHAR | oid::NAME | oid::UNKNOWN
    )
}

/// what a received value of a type must satisfy before a row is handed out
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    Any,
    Fixed(u32),
    Utf8,
    /// a version byte followed by UTF-8
    Jsonb,
}

pub(crate) fn shape(ty: Oid) -> Shape {
    match ty {
        oid::BOOL | oid::CHAR => Shape::Fixed(1),
        oid::INT2 => Shape::Fixed(2),
        oid::INT4 | oid::OID | oid::FLOAT4 => Shape::Fixed(4),
        oid::INT8 | oid::FLOAT8 => Shape::Fixed(8),
        oid::JSON => Shape::Utf8,
        oid::JSONB => Shape::Jsonb,
        ty if is_text(ty) => Shape::Utf8,
        _ => Shape::Any,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsNull {
    Yes,
    No,
}

pub const WRONG_TYPE: &str = "the value's type cannot be sent as this type";

/// a value that can be sent as a statement parameter in binary form
pub trait ToSql {
    /// appends the binary form for a parameter of type `ty`
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str>;
}

impl<T: ToSql + ?Sized> ToSql for &T {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        (**self).to_sql(ty, out)
    }
}

impl<T: ToSql> ToSql for Option<T> {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        match self {
            Some(value) => value.to_sql(ty, out),
            None => Ok(IsNull::Yes),
        }
    }
}

macro_rules! fixed_to_sql {
    ($($rust:ty => $oid:pat),* $(,)?) => {$(
        impl ToSql for $rust {
            fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
                if !matches!(ty, $oid) {
                    return Err(WRONG_TYPE);
                }
                out.extend_from_slice(&self.to_be_bytes());
                Ok(IsNull::No)
            }
        }
    )*};
}
fixed_to_sql!(i16 => oid::INT2, i32 => oid::INT4, i64 => oid::INT8, u32 => oid::OID,
    f32 => oid::FLOAT4, f64 => oid::FLOAT8, i8 => oid::CHAR);

impl ToSql for bool {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        if ty != oid::BOOL {
            return Err(WRONG_TYPE);
        }
        out.push(u8::from(*self));
        Ok(IsNull::No)
    }
}

impl ToSql for str {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        if !is_text(ty) {
            return Err(WRONG_TYPE);
        }
        out.extend_from_slice(self.as_bytes());
        Ok(IsNull::No)
    }
}

impl ToSql for String {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        self.as_str().to_sql(ty, out)
    }
}

impl ToSql for [u8] {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        if ty != oid::BYTEA {
            return Err(WRONG_TYPE);
        }
        out.extend_from_slice(self);
        Ok(IsNull::No)
    }
}

impl ToSql for Vec<u8> {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        self.as_slice().to_sql(ty, out)
    }
}

impl<const N: usize> ToSql for [u8; N] {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        self.as_slice().to_sql(ty, out)
    }
}

/// a one-dimensional array; `u8` has no `ToSql`, so byte slices stay `bytea`
impl<T: ToSql> ToSql for [T] {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        let element = element(ty).ok_or(WRONG_TYPE)?;
        let count = i32::try_from(self.len()).map_err(|_| "array too long")?;
        let dimensions = i32::from(count > 0);
        out.extend_from_slice(&dimensions.to_be_bytes());
        // the null flag is advisory; the server reads each element's length
        out.extend_from_slice(&1i32.to_be_bytes());
        out.extend_from_slice(&element.to_be_bytes());
        if count > 0 {
            out.extend_from_slice(&count.to_be_bytes());
            out.extend_from_slice(&1i32.to_be_bytes());
        }
        for value in self {
            write_value(value, element, out)?;
        }
        Ok(IsNull::No)
    }
}

impl<T: ToSql> ToSql for Vec<T> {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        self.as_slice().to_sql(ty, out)
    }
}

/// appends a length-prefixed value, the form parameters and array elements share
pub(crate) fn write_value(
    value: &(impl ToSql + ?Sized),
    ty: Oid,
    out: &mut Vec<u8>,
) -> Result<(), &'static str> {
    let at = out.len();
    out.extend_from_slice(&[0; 4]);
    let length = match value.to_sql(ty, out)? {
        IsNull::Yes => {
            out.truncate(at + 4);
            -1
        }
        IsNull::No => i32::try_from(out.len() - at - 4).map_err(|_| "value too long")?,
    };
    out[at..at + 4].copy_from_slice(&length.to_be_bytes());
    Ok(())
}

/// a value that can be read from a result column in binary form
///
/// `raw` has already been checked against the column type's fixed width or
/// text encoding where the type has one.
pub trait FromSql<'a>: Sized {
    fn accepts(ty: Oid) -> bool;
    fn from_sql(ty: Oid, raw: &'a [u8]) -> Result<Self, String>;
    fn from_null(_ty: Oid) -> Result<Self, String> {
        Err("unexpected NULL".into())
    }
}

impl<'a, T: FromSql<'a>> FromSql<'a> for Option<T> {
    fn accepts(ty: Oid) -> bool {
        T::accepts(ty)
    }
    fn from_sql(ty: Oid, raw: &'a [u8]) -> Result<Self, String> {
        T::from_sql(ty, raw).map(Some)
    }
    fn from_null(_ty: Oid) -> Result<Self, String> {
        Ok(None)
    }
}

macro_rules! fixed_from_sql {
    ($($rust:ty => $oid:pat),* $(,)?) => {$(
        impl FromSql<'_> for $rust {
            fn accepts(ty: Oid) -> bool {
                matches!(ty, $oid)
            }
            fn from_sql(_ty: Oid, raw: &[u8]) -> Result<Self, String> {
                match raw.try_into() {
                    Ok(bytes) => Ok(Self::from_be_bytes(bytes)),
                    Err(_) => Err("wrong width".into()),
                }
            }
        }
    )*};
}
fixed_from_sql!(i16 => oid::INT2, i32 => oid::INT4, i64 => oid::INT8, u32 => oid::OID,
    f32 => oid::FLOAT4, f64 => oid::FLOAT8, i8 => oid::CHAR);

impl FromSql<'_> for bool {
    fn accepts(ty: Oid) -> bool {
        ty == oid::BOOL
    }
    fn from_sql(_ty: Oid, raw: &[u8]) -> Result<Self, String> {
        match raw {
            [byte] => Ok(*byte != 0),
            _ => Err("wrong width".into()),
        }
    }
}

impl<'a> FromSql<'a> for &'a str {
    fn accepts(ty: Oid) -> bool {
        is_text(ty)
    }
    fn from_sql(_ty: Oid, raw: &'a [u8]) -> Result<Self, String> {
        // SAFETY: values of the accepted types were validated as UTF-8 when
        // the row was received
        Ok(unsafe { std::str::from_utf8_unchecked(raw) })
    }
}

impl FromSql<'_> for String {
    fn accepts(ty: Oid) -> bool {
        is_text(ty)
    }
    fn from_sql(ty: Oid, raw: &[u8]) -> Result<Self, String> {
        <&str>::from_sql(ty, raw).map(Into::into)
    }
}

impl<'a> FromSql<'a> for &'a [u8] {
    fn accepts(ty: Oid) -> bool {
        ty == oid::BYTEA
    }
    fn from_sql(_ty: Oid, raw: &'a [u8]) -> Result<Self, String> {
        Ok(raw)
    }
}

impl FromSql<'_> for Vec<u8> {
    fn accepts(ty: Oid) -> bool {
        ty == oid::BYTEA
    }
    fn from_sql(_ty: Oid, raw: &[u8]) -> Result<Self, String> {
        Ok(raw.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(value: &dyn ToSql, ty: Oid) -> Result<Vec<u8>, &'static str> {
        let mut out = Vec::new();
        write_value(&value, ty, &mut out).map(|()| out)
    }

    #[test]
    fn scalars_use_network_order_and_check_the_type() {
        assert_eq!(
            sent(&7i64, oid::INT8).unwrap(),
            [0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 7]
        );
        assert_eq!(
            sent(&-2i32, oid::INT4).unwrap(),
            [0, 0, 0, 4, 255, 255, 255, 254]
        );
        assert_eq!(sent(&true, oid::BOOL).unwrap(), [0, 0, 0, 1, 1]);
        assert_eq!(
            sent(&"hé", oid::TEXT).unwrap(),
            [0, 0, 0, 3, b'h', 0xc3, 0xa9]
        );
        assert_eq!(sent(&vec![1u8, 2], oid::BYTEA).unwrap(), [0, 0, 0, 2, 1, 2]);
        assert_eq!(sent(&None::<i64>, oid::INT8).unwrap(), [255, 255, 255, 255]);
        assert_eq!(sent(&7i64, oid::INT4), Err(WRONG_TYPE));
        assert_eq!(sent(&7i32, oid::INT8), Err(WRONG_TYPE));
        assert_eq!(sent(&"x", oid::BYTEA), Err(WRONG_TYPE));
        assert_eq!(sent(&vec![1u8], oid::TEXT), Err(WRONG_TYPE));
    }

    #[test]
    fn arrays_carry_their_element_type() {
        let values = vec!["a".to_owned(), "bc".to_owned()];
        assert_eq!(
            sent(&values, 1009).unwrap()[4..],
            [
                0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 25, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 1, b'a', 0,
                0, 0, 2, b'b', b'c'
            ]
        );
        let empty: &[i64] = &[];
        assert_eq!(
            sent(&empty, 1016).unwrap()[4..],
            [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 20]
        );
        assert_eq!(sent(&values, oid::TEXT), Err(WRONG_TYPE));
        assert_eq!(sent(&vec![1i64], 1009), Err(WRONG_TYPE));
    }

    #[test]
    fn values_decode_from_their_binary_form() {
        assert_eq!(i64::from_sql(oid::INT8, &[0, 0, 0, 0, 0, 0, 1, 0]), Ok(256));
        assert!(i64::from_sql(oid::INT8, &[0; 4]).is_err());
        assert_eq!(bool::from_sql(oid::BOOL, &[1]), Ok(true));
        assert!(bool::from_sql(oid::BOOL, &[]).is_err());
        assert_eq!(Option::<i32>::from_null(oid::INT4), Ok(None));
        assert!(i32::from_null(oid::INT4).is_err());
        assert!(<&str>::accepts(oid::NAME) && !<&str>::accepts(oid::BYTEA));
        assert!(!i64::accepts(oid::INT4) && Option::<i64>::accepts(oid::INT8));
    }
}
