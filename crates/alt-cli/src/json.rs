//! A tiny zero-dependency JSON writer for the `--json` output of native
//! commands. The schemas are small and fixed, so building a `Json` value tree
//! and serializing it compactly is enough — no serde, matching the project's
//! zero-dependency-by-default stance. This is the structured-I/O foundation
//! (VISION §4 A1): the JSON view is a first-class parallel to the human view.

use std::io::Write;

/// A JSON value. Strings carry raw bytes because repository paths need not be
/// UTF-8; serialization is lossy-UTF-8 (invalid sequences become U+FFFD) with
/// the standard JSON escapes, so the output is always valid JSON.
pub enum Json {
    Null,
    Bool(bool),
    Num(i64),
    Str(Vec<u8>),
    Array(Vec<Json>),
    Object(Vec<(&'static str, Json)>),
}

impl Json {
    /// A string value from any byte source (a `&str`, `&[u8]`, `Vec<u8>`, …).
    pub fn str(bytes: impl AsRef<[u8]>) -> Json {
        Json::Str(bytes.as_ref().to_vec())
    }

    /// Writes this value as compact JSON (no insignificant whitespace).
    pub fn write(&self, out: &mut (impl Write + ?Sized)) -> std::io::Result<()> {
        match self {
            Json::Null => out.write_all(b"null"),
            Json::Bool(b) => out.write_all(if *b { b"true" } else { b"false" }),
            Json::Num(n) => write!(out, "{n}"),
            Json::Str(bytes) => write_json_string(out, bytes),
            Json::Array(items) => {
                out.write_all(b"[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.write_all(b",")?;
                    }
                    item.write(out)?;
                }
                out.write_all(b"]")
            }
            Json::Object(fields) => {
                out.write_all(b"{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.write_all(b",")?;
                    }
                    write_json_string(out, k.as_bytes())?;
                    out.write_all(b":")?;
                    v.write(out)?;
                }
                out.write_all(b"}")
            }
        }
    }
}

/// Writes `{schema_version:1, <fields>}` as one compact JSON line — the shape
/// every native command's `--json` result takes.
pub fn emit(
    out: &mut (impl Write + ?Sized),
    fields: Vec<(&'static str, Json)>,
) -> std::io::Result<()> {
    let mut all = Vec::with_capacity(fields.len() + 1);
    all.push(("schema_version", Json::Num(1)));
    all.extend(fields);
    Json::Object(all).write(out)?;
    out.write_all(b"\n")
}

/// Writes `bytes` as a quoted JSON string: invalid UTF-8 is replaced (lossy),
/// `"`/`\` and the control characters get the standard JSON escapes.
fn write_json_string(out: &mut (impl Write + ?Sized), bytes: &[u8]) -> std::io::Result<()> {
    out.write_all(b"\"")?;
    for c in String::from_utf8_lossy(bytes).chars() {
        match c {
            '"' => out.write_all(b"\\\"")?,
            '\\' => out.write_all(b"\\\\")?,
            '\n' => out.write_all(b"\\n")?,
            '\r' => out.write_all(b"\\r")?,
            '\t' => out.write_all(b"\\t")?,
            '\x08' => out.write_all(b"\\b")?,
            '\x0c' => out.write_all(b"\\f")?,
            c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32)?,
            c => write!(out, "{c}")?,
        }
    }
    out.write_all(b"\"")
}
