use crate::{Map, Value};

/// Builds a [`Value`] from the same calls that produce text.
#[derive(Default)]
pub(super) struct Builder {
    open: Vec<Open>,
    done: Option<Value>,
}

enum Open {
    Array(Vec<Value>),
    Object(Map<String, Value>, Option<String>),
}

impl Builder {
    pub(super) fn put(&mut self, value: Value) {
        match self.open.last_mut() {
            Some(Open::Array(items)) => items.push(value),
            Some(Open::Object(map, key)) => {
                if let Some(key) = key.take() {
                    map.insert(key, value);
                }
            }
            None => self.done = Some(value),
        }
    }

    pub(super) fn begin_array(&mut self) {
        self.open.push(Open::Array(Vec::new()));
    }

    pub(super) fn begin_object(&mut self) {
        self.open.push(Open::Object(Map::new(), None));
    }

    pub(super) fn key(&mut self, name: String) {
        if let Some(Open::Object(_, key)) = self.open.last_mut() {
            *key = Some(name);
        }
    }

    pub(super) fn end(&mut self) {
        match self.open.pop() {
            Some(Open::Array(items)) => self.put(Value::Array(items)),
            Some(Open::Object(map, _)) => self.put(Value::Object(map)),
            None => {}
        }
    }

    pub(super) fn finish(self) -> Value {
        self.done.unwrap_or(Value::Null)
    }
}
