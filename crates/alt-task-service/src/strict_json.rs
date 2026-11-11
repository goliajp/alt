use alt_json::{Decoder, FromJson, Kind, Map, Value};

#[cfg(test)]
mod reference;

// Duplicate fields in maps and arbitrary context JSON are otherwise silently
// overwritten by Value/BTreeMap, even if the enclosing struct denies unknown
// fields. Reject them before decoding any record or computing its identity.
struct Strict(Value);

impl FromJson for Strict {
    fn from_json(d: &mut Decoder<'_>) -> alt_json::Result<Self> {
        match d.peek_kind()? {
            Kind::Array => d.seq("JSON with unique object keys", |s| {
                let mut items = Vec::new();
                while let Some(Strict(item)) = s.next_element()? {
                    items.push(item);
                }
                Ok(Strict(Value::Array(items)))
            }),
            Kind::Object => d.map("JSON with unique object keys", |m| {
                let mut map = Map::new();
                while let Some(key) = m.next_key(|key| Ok(key.to_owned()))? {
                    if map.contains_key(&key) {
                        return Err(alt_json::Error::custom("duplicate JSON key"));
                    }
                    let Strict(member) = m.next_value()?;
                    map.insert(key, member);
                }
                Ok(Strict(Value::Object(map)))
            }),
            _ => d.value().map(Strict),
        }
    }
}

pub(crate) fn decode(bytes: &[u8], limit: usize) -> crate::Result<Value> {
    if bytes.len() > limit {
        return Err(crate::Error::Limit("structured record bytes"));
    }
    let Strict(value) = alt_json::from_slice(bytes)?;
    Ok(value)
}
