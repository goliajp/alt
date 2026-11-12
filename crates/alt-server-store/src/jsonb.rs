//! JSON and JSONB values as the driver sends and receives them.

use alt_json::{FromJson, ToJson, Value};
use alt_json_serde::Serde;
use alt_pg::{FromSql, IsNull, Oid, ToSql, WRONG_TYPE, oid};

/// A `json` or `jsonb` parameter that is already JSON text, sent as it is.
#[derive(Debug)]
pub(crate) struct JsonText<'a>(pub &'a [u8]);

/// appends the header of a `json` or `jsonb` value's binary form
fn start(ty: Oid, out: &mut Vec<u8>) -> Result<(), &'static str> {
    match ty {
        oid::JSONB => out.push(JSONB_VERSION),
        oid::JSON => {}
        _ => return Err(WRONG_TYPE),
    }
    Ok(())
}

impl ToSql for JsonText<'_> {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        start(ty, out)?;
        out.extend_from_slice(self.0);
        Ok(IsNull::No)
    }
}

/// A `json` or `jsonb` parameter or column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Jsonb(pub Value);

/// The binary form of `jsonb` is this version byte followed by the text.
const JSONB_VERSION: u8 = 1;

impl Jsonb {
    pub fn from<T: ToJson + ?Sized>(value: &T) -> alt_json::Result<Self> {
        alt_json::to_value(value).map(Self)
    }

    pub fn read<T: FromJson>(self) -> alt_json::Result<T> {
        alt_json::from_value(self.0)
    }

    /// For the vendored contract types, which have serde implementations only.
    pub fn from_serde<T>(value: &T) -> alt_json::Result<Self>
    where
        for<'a> Serde<&'a T>: ToJson,
    {
        Self::from(&Serde(value))
    }

    pub fn read_serde<T>(self) -> alt_json::Result<T>
    where
        Serde<T>: FromJson,
    {
        self.read::<Serde<T>>().map(|Serde(value)| value)
    }
}

impl ToSql for Jsonb {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        start(ty, out)?;
        alt_json::to_writer(&mut *out, &self.0).map_err(|_| "value cannot be written as JSON")?;
        Ok(IsNull::No)
    }
}

impl FromSql<'_> for Jsonb {
    fn accepts(ty: Oid) -> bool {
        matches!(ty, oid::JSON | oid::JSONB)
    }

    fn from_sql(ty: Oid, raw: &[u8]) -> Result<Self, String> {
        let text = if ty == oid::JSONB {
            match raw.split_first() {
                Some((&JSONB_VERSION, text)) => text,
                _ => return Err("unsupported JSONB encoding version".into()),
            }
        } else {
            raw
        };
        alt_json::parse(text)
            .map(Self)
            .map_err(|error| error.to_string())
    }
}
