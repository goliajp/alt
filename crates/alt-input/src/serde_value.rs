//! Reads and writes a `serde_json::Value` field with alt-json, for as long
//! as the acceptance definition is still held as a serde tree.

use alt_json::{Decoder, Result, Writer};
use serde_json::Value;

pub(crate) fn to_json(value: &Value, w: &mut Writer) -> Result<()> {
    match value {
        Value::Null => w.null(),
        Value::Bool(b) => w.bool(*b),
        Value::Number(n) => match (n.as_u64(), n.as_i64()) {
            (Some(u), _) => w.u64(u),
            (_, Some(i)) => w.i64(i),
            _ => w.f64(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => w.string(s),
        Value::Array(items) => {
            w.begin_array()?;
            for item in items {
                w.element();
                to_json(item, w)?;
            }
            w.end_array();
            Ok(())
        }
        Value::Object(map) => {
            w.begin_object()?;
            for (key, member) in map {
                w.key(key);
                to_json(member, w)?;
            }
            w.end_object();
            Ok(())
        }
    }
}

pub(crate) fn from_json(d: &mut Decoder<'_>) -> Result<Value> {
    d.value().map(|value| convert(&value))
}

fn convert(value: &alt_json::Value) -> Value {
    match value {
        alt_json::Value::Null => Value::Null,
        alt_json::Value::Bool(b) => Value::Bool(*b),
        alt_json::Value::Number(n) => match (n.as_u64(), n.as_i64(), n.as_f64()) {
            (Some(u), _, _) => Value::from(u),
            (_, Some(i), _) => Value::from(i),
            (_, _, f) => Value::from(f.unwrap_or_default()),
        },
        alt_json::Value::String(s) => Value::String(s.clone()),
        alt_json::Value::Array(items) => Value::Array(items.iter().map(convert).collect()),
        alt_json::Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), convert(v))).collect())
        }
    }
}
