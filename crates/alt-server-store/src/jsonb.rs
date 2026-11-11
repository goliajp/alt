//! JSON and JSONB values as the driver sends and receives them.

use alt_json::{FromJson, ToJson, Value};
use alt_json_serde::Serde;
use postgres::types::private::BytesMut;
use postgres::types::{FromSql, IsNull, ToSql, Type, accepts, to_sql_checked};

type Failure = Box<dyn std::error::Error + Sync + Send>;

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
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Failure> {
        if *ty == Type::JSONB {
            out.extend_from_slice(&[JSONB_VERSION]);
        }
        out.extend_from_slice(&alt_json::to_vec(&self.0)?);
        Ok(IsNull::No)
    }

    accepts!(JSON, JSONB);
    to_sql_checked!();
}

impl<'a> FromSql<'a> for Jsonb {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Failure> {
        let text = if *ty == Type::JSONB {
            match raw.split_first() {
                Some((&JSONB_VERSION, text)) => text,
                _ => return Err("unsupported JSONB encoding version".into()),
            }
        } else {
            raw
        };
        Ok(Self(alt_json::parse(text)?))
    }

    accepts!(JSON, JSONB);
}
