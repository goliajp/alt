//! A JSON reader with just what the vendored test data needs.
use std::collections::BTreeMap;

#[derive(Debug)]
pub enum Json {
    Null,
    Bool(bool),
    Number,
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub fn load(name: &str) -> Json {
        let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read(&path).unwrap();
        let mut reader = Reader {
            bytes: &text,
            at: 0,
        };
        let value = reader.value();
        reader.space();
        assert_eq!(reader.at, text.len(), "trailing data in {path}");
        value
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members.get(key),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Json::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn items(&self) -> &[Json] {
        match self {
            Json::Array(items) => items,
            other => panic!("not an array: {other:?}"),
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn space(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn peek(&mut self) -> u8 {
        self.space();
        self.bytes[self.at]
    }

    fn expect(&mut self, byte: u8) {
        assert_eq!(self.peek(), byte, "at offset {}", self.at);
        self.at += 1;
    }

    fn literal(&mut self, word: &str, value: Json) -> Json {
        assert!(self.bytes[self.at..].starts_with(word.as_bytes()));
        self.at += word.len();
        value
    }

    fn list<T>(&mut self, close: u8, mut item: impl FnMut(&mut Self) -> T) -> Vec<T> {
        let mut items = Vec::new();
        if self.peek() != close {
            loop {
                items.push(item(self));
                if self.peek() != b',' {
                    break;
                }
                self.at += 1;
            }
        }
        self.expect(close);
        items
    }

    fn value(&mut self) -> Json {
        match self.peek() {
            b'{' => {
                self.at += 1;
                let members = self.list(b'}', |reader| {
                    let key = reader.string();
                    reader.expect(b':');
                    (key, reader.value())
                });
                Json::Object(members.into_iter().collect())
            }
            b'[' => {
                self.at += 1;
                Json::Array(self.list(b']', Self::value))
            }
            b'"' => Json::String(self.string()),
            b't' => self.literal("true", Json::Bool(true)),
            b'f' => self.literal("false", Json::Bool(false)),
            b'n' => self.literal("null", Json::Null),
            _ => {
                while b"+-.eE0123456789".contains(&self.bytes[self.at]) {
                    self.at += 1;
                }
                Json::Number
            }
        }
    }

    fn hex4(&mut self) -> u32 {
        let digits = std::str::from_utf8(&self.bytes[self.at..self.at + 4]).unwrap();
        self.at += 4;
        u32::from_str_radix(digits, 16).unwrap()
    }

    fn escape(&mut self) -> char {
        let escape = self.bytes[self.at];
        self.at += 1;
        match escape {
            b'"' | b'\\' | b'/' => escape as char,
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'u' => {
                let mut code = self.hex4();
                if (0xd800..0xdc00).contains(&code) {
                    assert!(self.bytes[self.at..].starts_with(b"\\u"));
                    self.at += 2;
                    code = 0x10000 + ((code - 0xd800) << 10) + (self.hex4() - 0xdc00);
                }
                char::from_u32(code).unwrap()
            }
            other => panic!("escape {other}"),
        }
    }

    fn string(&mut self) -> String {
        self.expect(b'"');
        let mut out = Vec::new();
        loop {
            let byte = self.bytes[self.at];
            self.at += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let mut buffer = [0u8; 4];
                    out.extend_from_slice(self.escape().encode_utf8(&mut buffer).as_bytes());
                }
                _ => out.push(byte),
            }
        }
        String::from_utf8(out).unwrap()
    }
}
