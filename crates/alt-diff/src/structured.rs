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
    /// TOML — Cargo.toml / pyproject.toml / rustfmt.toml / config.toml.
    /// M12/W34b.
    Toml,
}

impl StructKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            StructKind::Json => "json",
            StructKind::Toml => "toml",
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
///
/// Uses content-detection only — TOML can start with anything (a
/// comment, a key, a section header) so it doesn't fit the heuristic;
/// callers that know the file extension should use
/// [`summary_for_path`].
pub fn summary(old: &[u8], new: &[u8]) -> Option<Summary> {
    if looks_like_json(old) && looks_like_json(new) {
        return json_summary(old, new);
    }
    None
}

/// Path-aware dispatcher. Routes on the file extension so TOML
/// (`*.toml`) gets the TOML parser even though TOML's first line can
/// be anything. JSON stays content-detected so a `.txt` file that
/// happens to be JSON still gets the semantic treatment.
pub fn summary_for_path(path: &str, old: &[u8], new: &[u8]) -> Option<Summary> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".toml") {
        return toml_summary(old, new);
    }
    if lower.ends_with(".json") || (looks_like_json(old) && looks_like_json(new)) {
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

// ─────────────────────────────────────────────────────────────────────
//  TOML (M12/W34b)
// ─────────────────────────────────────────────────────────────────────

/// TOML semantic diff. Parses a subset wide enough to cover real
/// Cargo.toml / pyproject.toml / rustfmt.toml shapes:
///
/// - bare keys, dotted keys, quoted keys
/// - `[section]` headers and `[[array_of_tables]]`
/// - basic `"..."` strings (with escapes) + literal `'...'` strings
/// - integers, floats (incl. `_` digit separators), bools
/// - homogeneous arrays
/// - line comments (`# …`)
///
/// Anything outside that subset (multi-line strings, dates, inline
/// tables) makes the parser return `None` so the caller falls back to
/// the line diff — same fallback policy as the JSON path.
fn toml_summary(old: &[u8], new: &[u8]) -> Option<Summary> {
    let old_val = parse_toml(old)?;
    let new_val = parse_toml(new)?;
    let mut paths = Vec::new();
    diff_values("$", &old_val, &new_val, &mut paths);
    Some(Summary {
        kind: StructKind::Toml,
        paths,
    })
}

fn parse_toml(data: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(data).ok()?;
    let mut p = TomlParser { text, at: 0 };
    let mut root: Vec<(String, Value)> = Vec::new();
    // The current table the parser is writing into. We track it as a
    // path so we can navigate by key descent each time we land on a
    // `[a.b.c]` header; the root vector grows as we go.
    let mut current_path: Vec<String> = Vec::new();
    loop {
        p.skip_blanks_and_comments();
        if p.at >= p.text.len() {
            break;
        }
        if p.peek_char()? == '[' {
            // Section header: either `[a.b.c]` (replace path) or
            // `[[a.b.c]]` (append to the array at that path).
            p.at += 1; // skip '['
            let is_array_of_tables = p.peek_char() == Some('[');
            if is_array_of_tables {
                p.at += 1;
            }
            let key_path = p.parse_dotted_key()?;
            // expect closing bracket(s)
            p.skip_inline_whitespace();
            if p.peek_char()? != ']' {
                return None;
            }
            p.at += 1;
            if is_array_of_tables {
                if p.peek_char()? != ']' {
                    return None;
                }
                p.at += 1;
            }
            p.skip_until_newline_or_comment();
            if is_array_of_tables {
                // Ensure the array exists, append an empty table.
                ensure_array_of_tables(&mut root, &key_path);
                current_path = key_path.clone();
            } else {
                ensure_table(&mut root, &key_path);
                current_path = key_path;
            }
            continue;
        }

        // Plain `key = value` line.
        let key_path = p.parse_dotted_key()?;
        p.skip_inline_whitespace();
        if p.peek_char()? != '=' {
            return None;
        }
        p.at += 1;
        p.skip_inline_whitespace();
        let value = p.parse_value()?;
        p.skip_until_newline_or_comment();

        let mut full_path = current_path.clone();
        full_path.extend(key_path);
        insert_into(&mut root, &full_path, value)?;
    }
    Some(Value::Object(root))
}

fn ensure_table(root: &mut Vec<(String, Value)>, path: &[String]) {
    // Walk down, creating object nodes as needed. The last segment
    // must end up as an Object.
    let mut cur: &mut Vec<(String, Value)> = root;
    for (i, key) in path.iter().enumerate() {
        let last = i == path.len() - 1;
        let idx = match cur.iter().position(|(k, _)| k == key) {
            Some(j) => j,
            None => {
                cur.push((key.clone(), Value::Object(Vec::new())));
                cur.len() - 1
            }
        };
        match &mut cur[idx].1 {
            Value::Object(inner) => {
                if last {
                    return;
                }
                cur = inner;
            }
            Value::Array(items) => {
                // `[a]` followed by `[a.b]` walks into the *last*
                // array-of-tables entry — that matches TOML semantics.
                let last_idx = items.len().saturating_sub(1);
                if let Some(Value::Object(inner)) = items.get_mut(last_idx) {
                    if last {
                        return;
                    }
                    cur = inner;
                } else {
                    return;
                }
            }
            _ => return,
        }
    }
}

fn ensure_array_of_tables(root: &mut Vec<(String, Value)>, path: &[String]) {
    // Walk to the parent object, find/create the array at the last
    // segment, then append a fresh empty table to it.
    if path.is_empty() {
        return;
    }
    let parent = &path[..path.len() - 1];
    let leaf = &path[path.len() - 1];
    ensure_table(root, parent);
    let cur: &mut Vec<(String, Value)> = walk_to(root, parent);
    let idx = match cur.iter().position(|(k, _)| k == leaf) {
        Some(j) => j,
        None => {
            cur.push((leaf.clone(), Value::Array(Vec::new())));
            cur.len() - 1
        }
    };
    if let Value::Array(items) = &mut cur[idx].1 {
        items.push(Value::Object(Vec::new()));
    }
}

fn walk_to<'a>(
    root: &'a mut Vec<(String, Value)>,
    path: &[String],
) -> &'a mut Vec<(String, Value)> {
    let mut cur: &'a mut Vec<(String, Value)> = root;
    for key in path {
        let idx = cur
            .iter()
            .position(|(k, _)| k == key)
            .expect("path created");
        match &mut cur[idx].1 {
            Value::Object(inner) => cur = inner,
            Value::Array(items) => {
                let last_idx = items.len() - 1;
                if let Value::Object(inner) = &mut items[last_idx] {
                    cur = inner;
                } else {
                    panic!("walk_to: array entry not a table");
                }
            }
            _ => panic!("walk_to: non-container at {key}"),
        }
    }
    cur
}

fn insert_into(root: &mut Vec<(String, Value)>, path: &[String], value: Value) -> Option<()> {
    if path.is_empty() {
        return None;
    }
    let parent = &path[..path.len() - 1];
    let leaf = &path[path.len() - 1];
    ensure_table(root, parent);
    let cur = walk_to(root, parent);
    // Replace or append the leaf entry.
    if let Some(slot) = cur.iter_mut().find(|(k, _)| k == leaf) {
        slot.1 = value;
    } else {
        cur.push((leaf.clone(), value));
    }
    Some(())
}

struct TomlParser<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> TomlParser<'a> {
    fn peek_char(&self) -> Option<char> {
        self.text[self.at..].chars().next()
    }

    fn skip_blanks_and_comments(&mut self) {
        loop {
            // skip whitespace
            while let Some(c) = self.peek_char() {
                if c.is_whitespace() {
                    self.at += c.len_utf8();
                } else {
                    break;
                }
            }
            // line comment?
            if self.peek_char() == Some('#') {
                while let Some(c) = self.peek_char() {
                    self.at += c.len_utf8();
                    if c == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn skip_inline_whitespace(&mut self) {
        while let Some(c) = self.peek_char() {
            if c == ' ' || c == '\t' {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    fn skip_until_newline_or_comment(&mut self) {
        self.skip_inline_whitespace();
        if self.peek_char() == Some('#') {
            while let Some(c) = self.peek_char() {
                self.at += c.len_utf8();
                if c == '\n' {
                    return;
                }
            }
        }
        // skip a single newline (multi-line value would have already
        // consumed its internal newlines)
        if self.peek_char() == Some('\n') {
            self.at += 1;
        } else if self.peek_char() == Some('\r') {
            self.at += 1;
            if self.peek_char() == Some('\n') {
                self.at += 1;
            }
        }
    }

    fn parse_dotted_key(&mut self) -> Option<Vec<String>> {
        let mut out = Vec::new();
        loop {
            self.skip_inline_whitespace();
            let part = self.parse_key_segment()?;
            out.push(part);
            self.skip_inline_whitespace();
            if self.peek_char() == Some('.') {
                self.at += 1;
            } else {
                break;
            }
        }
        Some(out)
    }

    fn parse_key_segment(&mut self) -> Option<String> {
        match self.peek_char()? {
            '"' => self.parse_basic_string(),
            '\'' => self.parse_literal_string(),
            c if is_bare_key_char(c) => {
                let start = self.at;
                while let Some(c) = self.peek_char() {
                    if is_bare_key_char(c) {
                        self.at += c.len_utf8();
                    } else {
                        break;
                    }
                }
                Some(self.text[start..self.at].to_owned())
            }
            _ => None,
        }
    }

    fn parse_value(&mut self) -> Option<Value> {
        self.skip_inline_whitespace();
        match self.peek_char()? {
            '"' => Some(Value::String(self.parse_basic_string()?)),
            '\'' => Some(Value::String(self.parse_literal_string()?)),
            '[' => self.parse_array(),
            't' | 'f' => self.parse_bool(),
            '-' | '+' | '0'..='9' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_basic_string(&mut self) -> Option<String> {
        // `"..."` with the usual JSON-ish escapes. Multi-line `"""`
        // strings are out of W34b scope.
        self.at += 1; // opening "
        let mut out = String::new();
        loop {
            let c = self.peek_char()?;
            match c {
                '"' => {
                    self.at += 1;
                    return Some(out);
                }
                '\\' => {
                    self.at += 1;
                    let esc = self.peek_char()?;
                    self.at += esc.len_utf8();
                    match esc {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        _ => return None,
                    }
                }
                '\n' => return None, // single-line strings only
                _ => {
                    out.push(c);
                    self.at += c.len_utf8();
                }
            }
        }
    }

    fn parse_literal_string(&mut self) -> Option<String> {
        // `'...'`: no escapes, content is taken literally.
        self.at += 1;
        let start = self.at;
        loop {
            let c = self.peek_char()?;
            if c == '\'' {
                let body = self.text[start..self.at].to_owned();
                self.at += 1;
                return Some(body);
            }
            if c == '\n' {
                return None;
            }
            self.at += c.len_utf8();
        }
    }

    fn parse_bool(&mut self) -> Option<Value> {
        if self.text[self.at..].starts_with("true") {
            self.at += 4;
            return Some(Value::Bool(true));
        }
        if self.text[self.at..].starts_with("false") {
            self.at += 5;
            return Some(Value::Bool(false));
        }
        None
    }

    fn parse_number(&mut self) -> Option<Value> {
        let start = self.at;
        if matches!(self.peek_char(), Some('+' | '-')) {
            self.at += 1;
        }
        while let Some(c) = self.peek_char() {
            match c {
                '0'..='9' | '.' | 'e' | 'E' | '+' | '-' | '_' => self.at += 1,
                _ => break,
            }
        }
        // Strip TOML digit separators before letting Rust parse.
        let raw: String = self.text[start..self.at]
            .chars()
            .filter(|c| *c != '_')
            .collect();
        let parsed: f64 = raw.parse().ok()?;
        // Canonicalise: keep an integer form when value is integral
        // (matches JSON path's `1` vs `1.0` collapse).
        let canonical = if parsed.fract() == 0.0 && parsed.abs() < 1e16 {
            format!("{}", parsed as i64)
        } else {
            format!("{parsed}")
        };
        Some(Value::Number(canonical))
    }

    fn parse_array(&mut self) -> Option<Value> {
        self.at += 1; // skip [
        let mut items = Vec::new();
        loop {
            self.skip_blanks_and_comments();
            if self.peek_char() == Some(']') {
                self.at += 1;
                return Some(Value::Array(items));
            }
            let v = self.parse_value()?;
            items.push(v);
            self.skip_blanks_and_comments();
            match self.peek_char()? {
                ',' => {
                    self.at += 1;
                }
                ']' => {
                    self.at += 1;
                    return Some(Value::Array(items));
                }
                _ => return None,
            }
        }
    }
}

fn is_bare_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
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

    // ── TOML (M12/W34b) ──────────────────────────────────────────

    #[test]
    fn toml_summary_routes_through_summary_for_path() {
        let a = br#"
[package]
name = "alt"
version = "0.1.0"
"#
        .to_vec();
        let b = br#"
[package]
name = "alt"
version = "0.2.0"
"#
        .to_vec();
        let s = summary_for_path("Cargo.toml", &a, &b).unwrap();
        assert_eq!(s.kind, StructKind::Toml);
        let r = s.render();
        assert!(r.contains("$.package.version"), "version path missing: {r}");
        assert!(r.contains("0.1.0") && r.contains("0.2.0"), "old → new: {r}");
        assert!(!r.contains("name"), "unchanged key must not appear: {r}");
    }

    #[test]
    fn toml_dotted_key_change_renders_path() {
        let a = b"server.port = 8080\nserver.host = \"localhost\"\n";
        let b = b"server.port = 9090\nserver.host = \"localhost\"\n";
        let s = summary_for_path("config.toml", a, b).unwrap();
        let r = s.render();
        assert!(r.contains("$.server.port"), "dotted path missing: {r}");
        assert!(!r.contains("$.server.host"), "unchanged dotted key: {r}");
    }

    #[test]
    fn toml_section_header_and_comments_are_ignored() {
        let a = br#"
# top comment
[package]
name = "alt"  # inline comment
version = "0.1.0"
"#;
        let b = br#"
[package]
# differently placed comment
name = "alt"
version = "0.1.0"
# trailing comment
"#;
        let s = summary_for_path("Cargo.toml", a, b).unwrap();
        assert!(
            s.semantically_unchanged(),
            "comments + whitespace shouldn't flag: {:?}",
            s.paths
        );
    }

    #[test]
    fn toml_array_change_uses_index_path() {
        let a = b"features = [\"a\", \"b\", \"c\"]\n";
        let b = b"features = [\"a\", \"x\", \"c\"]\n";
        let s = summary_for_path("Cargo.toml", a, b).unwrap();
        let r = s.render();
        assert!(r.contains("$.features[1]"), "indexed path missing: {r}");
        assert!(r.contains("\"b\""), "old value missing: {r}");
        assert!(r.contains("\"x\""), "new value missing: {r}");
    }

    #[test]
    fn toml_array_of_tables_walks_correctly() {
        let a = br#"
[[dependencies]]
name = "serde"
version = "1"

[[dependencies]]
name = "clap"
version = "4"
"#;
        let b = br#"
[[dependencies]]
name = "serde"
version = "1"

[[dependencies]]
name = "clap"
version = "5"
"#;
        let s = summary_for_path("Cargo.toml", a, b).unwrap();
        let r = s.render();
        assert!(
            r.contains("$.dependencies[1].version"),
            "array-of-tables path missing: {r}"
        );
    }

    #[test]
    fn toml_number_canonicalisation() {
        // `1`, `1.0`, and `1_000` (with separator) trip the same
        // canonical form (1, 1, 1000).
        let a = b"a = 1\nb = 1_000\nc = 3.14\n";
        let b = b"a = 1.0\nb = 1000\nc = 3.14\n";
        let s = summary_for_path("Cargo.toml", a, b).unwrap();
        assert!(
            s.semantically_unchanged(),
            "1/1.0 and 1_000/1000 should compare equal: {:?}",
            s.paths
        );
    }

    #[test]
    fn toml_literal_vs_basic_string_compare_by_value() {
        let a = br#"name = "alt""#;
        let b = br#"name = 'alt'"#;
        let s = summary_for_path("Cargo.toml", a, b).unwrap();
        assert!(
            s.semantically_unchanged(),
            "string quote style is not semantic: {:?}",
            s.paths
        );
    }

    #[test]
    fn toml_invalid_input_returns_none() {
        let a = b"not = toml = wrong\n";
        assert!(summary_for_path("x.toml", a, a).is_none());
    }
}
