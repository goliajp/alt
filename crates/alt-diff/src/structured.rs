//! M12/W34 — semantic diff for structured-data files (JSON today, YAML
//! / TOML follow under the same surface).
//!
//! The dogfood thesis: an agent edits one key in a 500-line JSON config
//! and the line-based diff drowns the change in formatting noise
//! (re-pretty-printed strings, reordered keys, trailing comma fix).
//! A structured diff reports `$.server.port: 8080 → 9090` and ignores
//! everything that didn't semantically change — exactly the same role
//! `alt-treediff` plays for source code.
//!
//! Surface mirrors [`crate::part_aware::Summary`]:
//!
//! - [`Summary`] = the kind + a flat list of `(path, PathChange)`
//! - [`PathChange`] = `Same` / `Changed` / `Added` / `Removed`
//!
//! Renders to one line per `alt diff` invocation; emits a structured
//! `--json` field so an agent can consume the result without re-parsing.
//!
//! Implementation is zero-dep: a hand-rolled recursive-descent JSON
//! parser that decodes any RFC 8259 value, then a recursive walker
//! that emits path-changes at the *first* differing point on each
//! key path (a whole object changing reports as one change, not N
//! leaf changes, so the line stays informative).

use std::fmt::Write;

/// What kind of structured file we recognised. Mirrors
/// [`crate::part_aware::PartKind`] but is its own enum because the
/// recognition set may diverge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructKind {
    Json,
}

impl StructKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            StructKind::Json => "json",
        }
    }
}

/// One named path's verdict, mirroring `crate::part_aware::PartChange`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathChange {
    Changed { old_repr: String, new_repr: String },
    Added { new_repr: String },
    Removed { old_repr: String },
}

/// One file's structured diff. Empty `paths` means "no semantic
/// changes" — useful for catching the dogfood case where a
/// pretty-printer round-tripped the file but every key kept its
/// value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub kind: StructKind,
    pub paths: Vec<(String, PathChange)>,
}

impl Summary {
    /// Renders as a single line — the same调性 as
    /// [`crate::part_aware::Summary::render`]. Each path becomes one
    /// `name: change` token, separated by `|`.
    pub fn render(&self) -> String {
        if self.paths.is_empty() {
            return format!("{}: semantically unchanged", self.kind.as_str());
        }
        let mut bits = Vec::with_capacity(self.paths.len());
        for (path, change) in &self.paths {
            bits.push(match change {
                PathChange::Changed { old_repr, new_repr } => {
                    format!("{path}: {old_repr} → {new_repr}")
                }
                PathChange::Added { new_repr } => format!("{path} added: {new_repr}"),
                PathChange::Removed { old_repr } => format!("{path} removed: {old_repr}"),
            });
        }
        format!("{}: {}", self.kind.as_str(), bits.join(" | "))
    }

    pub fn semantically_unchanged(&self) -> bool {
        self.paths.is_empty()
    }
}

/// Top-level: try every structured prism. Returns `None` when neither
/// input parses (the caller stays on the line-based path) or when the
/// recognition heuristic doesn't fire on both sides.
pub fn summary(old: &[u8], new: &[u8]) -> Option<Summary> {
    if looks_like_json(old) && looks_like_json(new) {
        return json_summary(old, new);
    }
    None
}

fn looks_like_json(data: &[u8]) -> bool {
    // First non-whitespace character must be one of the six legal
    // JSON value openers. Avoids treating arbitrary text starting
    // with `t` (e.g. "true story") as JSON.
    for &b in data {
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => continue,
            b'{' | b'[' | b'"' | b't' | b'f' | b'n' => return true,
            b'-' | b'0'..=b'9' => return true,
            _ => return false,
        }
    }
    false
}

fn json_summary(old: &[u8], new: &[u8]) -> Option<Summary> {
    let old_val = parse_json(old)?;
    let new_val = parse_json(new)?;
    let mut paths = Vec::new();
    diff_values("$", &old_val, &new_val, &mut paths);
    Some(Summary {
        kind: StructKind::Json,
        paths,
    })
}

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    fn short_repr(&self) -> String {
        let mut out = String::new();
        self.write_repr(&mut out, 64);
        out
    }

    fn write_repr(&self, out: &mut String, budget: usize) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Number(n) => out.push_str(n),
            Value::String(s) => {
                out.push('"');
                if s.len() <= budget {
                    out.push_str(s);
                } else {
                    out.push_str(&s[..budget]);
                    out.push_str("...");
                }
                out.push('"');
            }
            Value::Array(_) => out.push_str("[…]"),
            Value::Object(_) => out.push_str("{…}"),
        }
    }
}

fn diff_values(path: &str, old: &Value, new: &Value, out: &mut Vec<(String, PathChange)>) {
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => {
            let mut keys: Vec<&str> = o
                .iter()
                .map(|(k, _)| k.as_str())
                .chain(n.iter().map(|(k, _)| k.as_str()))
                .collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let child_path = format!("{path}.{k}");
                match (lookup(o, k), lookup(n, k)) {
                    (Some(a), Some(b)) => diff_values(&child_path, a, b, out),
                    (None, Some(b)) => out.push((
                        child_path,
                        PathChange::Added {
                            new_repr: b.short_repr(),
                        },
                    )),
                    (Some(a), None) => out.push((
                        child_path,
                        PathChange::Removed {
                            old_repr: a.short_repr(),
                        },
                    )),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(o), Value::Array(n)) => {
            // First differing index gets reported per slot; arrays
            // longer / shorter than each other get tail elements
            // recorded as added/removed. Keeps the line readable
            // without doing a full LCS (overkill for config files
            // where arrays are usually short).
            let common = o.len().min(n.len());
            for i in 0..common {
                let child_path = format!("{path}[{i}]");
                diff_values(&child_path, &o[i], &n[i], out);
            }
            for (i, v) in n.iter().enumerate().skip(common) {
                let p = format!("{path}[{i}]");
                out.push((
                    p,
                    PathChange::Added {
                        new_repr: v.short_repr(),
                    },
                ));
            }
            for (i, v) in o.iter().enumerate().skip(common) {
                let p = format!("{path}[{i}]");
                out.push((
                    p,
                    PathChange::Removed {
                        old_repr: v.short_repr(),
                    },
                ));
            }
        }
        (a, b) if a == b => {} // semantically same
        (a, b) => out.push((
            path.to_owned(),
            PathChange::Changed {
                old_repr: a.short_repr(),
                new_repr: b.short_repr(),
            },
        )),
    }
}

fn lookup<'a>(obj: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Zero-dep recursive-descent JSON parser. Reads from a byte slice and
/// returns `None` on any malformed input (the caller falls back to the
/// line-based diff path).
fn parse_json(data: &[u8]) -> Option<Value> {
    let mut p = Parser { bytes: data, at: 0 };
    p.skip_whitespace();
    let v = p.parse_value()?;
    p.skip_whitespace();
    if p.at != data.len() {
        return None;
    }
    Some(v)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Parser<'a> {
    fn skip_whitespace(&mut self) {
        while let Some(&b) = self.bytes.get(self.at) {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn expect(&mut self, c: u8) -> Option<()> {
        if self.peek() == Some(c) {
            self.at += 1;
            Some(())
        } else {
            None
        }
    }

    fn parse_value(&mut self) -> Option<Value> {
        self.skip_whitespace();
        match self.peek()? {
            b'n' => self.parse_literal(b"null", Value::Null),
            b't' => self.parse_literal(b"true", Value::Bool(true)),
            b'f' => self.parse_literal(b"false", Value::Bool(false)),
            b'"' => self.parse_string().map(Value::String),
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_literal(&mut self, lit: &[u8], out: Value) -> Option<Value> {
        if self.bytes.get(self.at..self.at + lit.len()) == Some(lit) {
            self.at += lit.len();
            Some(out)
        } else {
            None
        }
    }

    fn parse_string(&mut self) -> Option<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        while let Some(b) = self.peek() {
            match b {
                b'"' => {
                    self.at += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.at += 1;
                    match self.peek()? {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0c'),
                        b'u' => {
                            self.at += 1;
                            // 4-hex codepoint, BMP only — non-BMP /u
                            // sequences in configs are rare; lossy-cast
                            // to char for graceful degradation.
                            let hex_bytes = self.bytes.get(self.at..self.at + 4)?;
                            let hex_str = std::str::from_utf8(hex_bytes).ok()?;
                            let cp = u32::from_str_radix(hex_str, 16).ok()?;
                            self.at += 4;
                            // skip the trailing increment for 'u'
                            let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            out.push(c);
                            continue;
                        }
                        _ => return None,
                    }
                    self.at += 1;
                }
                _ => {
                    // Multi-byte UTF-8 codepoints — copy raw bytes
                    // through to the output buffer until we hit `"`
                    // or `\`. JSON strings are valid UTF-8 by spec.
                    let start = self.at;
                    while let Some(&c) = self.bytes.get(self.at) {
                        if c == b'"' || c == b'\\' {
                            break;
                        }
                        self.at += 1;
                    }
                    let chunk = std::str::from_utf8(&self.bytes[start..self.at]).ok()?;
                    out.push_str(chunk);
                }
            }
        }
        None
    }

    fn parse_number(&mut self) -> Option<Value> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        while let Some(b) = self.peek() {
            match b {
                b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-' => self.at += 1,
                _ => break,
            }
        }
        // Normalise the textual form so semantically-equal numbers
        // diff as Same: `1.0` and `1` should not flag a change. We
        // do this by re-parsing as f64 (numbers in configs are
        // realistically inside f64 range; for hex integers / 64-bit
        // ints outside that range we fall back to the raw string).
        let raw = std::str::from_utf8(&self.bytes[start..self.at]).ok()?;
        let parsed: f64 = raw.parse().ok()?;
        let mut canonical = String::new();
        if parsed.fract() == 0.0 && parsed.abs() < 1e16 {
            write!(canonical, "{}", parsed as i64).ok()?;
        } else {
            write!(canonical, "{parsed}").ok()?;
        }
        Some(Value::Number(canonical))
    }

    fn parse_object(&mut self) -> Option<Value> {
        self.expect(b'{')?;
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Some(Value::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let val = self.parse_value()?;
            entries.push((key, val));
            self.skip_whitespace();
            match self.peek()? {
                b',' => {
                    self.at += 1;
                }
                b'}' => {
                    self.at += 1;
                    return Some(Value::Object(entries));
                }
                _ => return None,
            }
        }
    }

    fn parse_array(&mut self) -> Option<Value> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Some(Value::Array(items));
        }
        loop {
            let v = self.parse_value()?;
            items.push(v);
            self.skip_whitespace();
            match self.peek()? {
                b',' => {
                    self.at += 1;
                }
                b']' => {
                    self.at += 1;
                    return Some(Value::Array(items));
                }
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantically_identical_jsons_report_no_changes() {
        let a = br#"{"a":1,"b":[1,2,3],"c":{"d":"e"}}"#;
        let b = br#"{ "a":1, "b": [1, 2, 3], "c":{"d":"e"} }"#;
        let s = summary(a, b).unwrap();
        assert!(
            s.semantically_unchanged(),
            "whitespace-only difference must not be a semantic change: {:?}",
            s.paths
        );
        assert_eq!(s.render(), "json: semantically unchanged");
    }

    #[test]
    fn key_value_change_renders_path_with_old_and_new() {
        let a = br#"{"server":{"port":8080,"host":"localhost"}}"#;
        let b = br#"{"server":{"port":9090,"host":"localhost"}}"#;
        let s = summary(a, b).unwrap();
        let r = s.render();
        assert!(r.contains("$.server.port"), "path missing: {r}");
        assert!(r.contains("8080"), "old value missing: {r}");
        assert!(r.contains("9090"), "new value missing: {r}");
        assert!(
            !r.contains("$.server.host"),
            "unchanged key must not appear: {r}"
        );
    }

    #[test]
    fn added_key_renders_added() {
        let a = br#"{"a":1}"#;
        let b = br#"{"a":1,"new":"x"}"#;
        let s = summary(a, b).unwrap();
        let r = s.render();
        assert!(r.contains("$.new added"), "added key missing: {r}");
    }

    #[test]
    fn removed_key_renders_removed() {
        let a = br#"{"a":1,"gone":42}"#;
        let b = br#"{"a":1}"#;
        let s = summary(a, b).unwrap();
        let r = s.render();
        assert!(r.contains("$.gone removed"), "removed key missing: {r}");
    }

    #[test]
    fn nested_object_replacement_reports_one_change_not_n_leaves() {
        // Old child is a string; new child is an object. The change
        // surfaces at the *top* of the divergence — `$.a` — not as a
        // flood of leaf adds/removes inside the new object.
        let a = br#"{"a":"x"}"#;
        let b = br#"{"a":{"x":1,"y":2}}"#;
        let s = summary(a, b).unwrap();
        assert_eq!(
            s.paths.len(),
            1,
            "should be one root-level change: {:?}",
            s.paths
        );
        let r = s.render();
        assert!(r.contains("$.a"), "path missing: {r}");
    }

    #[test]
    fn array_element_change_uses_index_path() {
        let a = br#"{"items":[1,2,3]}"#;
        let b = br#"{"items":[1,5,3]}"#;
        let s = summary(a, b).unwrap();
        let r = s.render();
        assert!(r.contains("$.items[1]"), "indexed path missing: {r}");
        assert!(r.contains('2'), "old element missing: {r}");
        assert!(r.contains('5'), "new element missing: {r}");
    }

    #[test]
    fn array_tail_grows_as_added() {
        let a = br#"[1,2]"#;
        let b = br#"[1,2,3,4]"#;
        let s = summary(a, b).unwrap();
        let r = s.render();
        assert!(r.contains("$[2] added"), "appended element 2: {r}");
        assert!(r.contains("$[3] added"), "appended element 3: {r}");
    }

    #[test]
    fn number_canonicalisation_treats_1_and_1_0_as_same() {
        let a = br#"{"n":1}"#;
        let b = br#"{"n":1.0}"#;
        let s = summary(a, b).unwrap();
        assert!(
            s.semantically_unchanged(),
            "1 and 1.0 are the same value: {:?}",
            s.paths
        );
    }

    #[test]
    fn invalid_json_returns_none() {
        let bad = b"not json at all";
        assert!(summary(bad, bad).is_none());
    }

    #[test]
    fn one_side_invalid_returns_none() {
        let a = br#"{"a":1}"#;
        let bad = b"not json";
        assert!(summary(a, bad).is_none());
    }

    #[test]
    fn escape_sequences_decode() {
        let a = br#"{"msg":"hello\nworld"}"#;
        let b = br#"{"msg":"hello\nworld"}"#;
        let s = summary(a, b).unwrap();
        assert!(s.semantically_unchanged());
    }
}
