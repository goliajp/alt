//! Reads and writes the `alt_json::Value` field through serde, for the
//! crates that still read and write the manifest types that way.

use std::fmt;

use alt_json::{Map, Value};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserializer, Serializer};

struct Tree<'a>(&'a Value);

impl serde::Serialize for Tree<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize(self.0, serializer)
    }
}

pub(crate) fn serialize<S: Serializer>(value: &Value, serializer: S) -> Result<S::Ok, S::Error> {
    match value {
        Value::Null => serializer.serialize_unit(),
        Value::Bool(b) => serializer.serialize_bool(*b),
        Value::Number(n) => match (n.as_u64(), n.as_i64()) {
            (Some(u), _) => serializer.serialize_u64(u),
            (_, Some(i)) => serializer.serialize_i64(i),
            _ => serializer.serialize_f64(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => serializer.serialize_str(s),
        Value::Array(items) => {
            let mut seq = serializer.serialize_seq(Some(items.len()))?;
            for item in items {
                seq.serialize_element(&Tree(item))?;
            }
            seq.end()
        }
        Value::Object(map) => {
            let mut object = serializer.serialize_map(Some(map.len()))?;
            for (key, member) in map {
                object.serialize_entry(key, &Tree(member))?;
            }
            object.end()
        }
    }
}

struct Owned(Value);

impl<'de> serde::Deserialize<'de> for Owned {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize(deserializer).map(Owned)
    }
}

struct Values;

impl<'de> Visitor<'de> for Values {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any valid JSON value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Owned(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Value, A::Error> {
        let mut map = Map::new();
        while let Some((key, Owned(member))) = object.next_entry::<String, Owned>()? {
            map.insert(key, member);
        }
        Ok(Value::Object(map))
    }
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Value, D::Error> {
    deserializer.deserialize_any(Values)
}
