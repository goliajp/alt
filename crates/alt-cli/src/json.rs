//! A tiny zero-dependency JSON writer for the `--json` output of native
//! commands. The schemas are small and fixed, so building a `Json` value tree
//! and serializing it compactly is enough — no serde, matching the project's
//! zero-dependency-by-default stance. This is the structured-I/O foundation:
//! the JSON view is a first-class parallel to the human view.

use std::io::Write;

/// A JSON value. Strings carry raw bytes because repository paths need not be
/// UTF-8; serialization is lossy-UTF-8 (invalid sequences become U+FFFD) with
/// the standard JSON escapes, so the output is always valid JSON.
pub enum Json {
    Null,
    Bool(bool),
    Num(i64),
    /// A finite floating-point number. NaN/Infinity collapse to 0.0 on
    /// serialization (JSON has no representation for them), so values that
    /// could ever be non-finite must be sanitized at construction.
    Float(f64),
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
            Json::Float(f) => {
                if !f.is_finite() {
                    out.write_all(b"0")
                } else if *f == f.trunc() && f.abs() < 1e15 {
                    // integer-valued: print without trailing `.0` so e.g.
                    // `0.0` and `1.0` stay compact and obviously bounded
                    write!(out, "{}", *f as i64)
                } else {
                    write!(out, "{f}")
                }
            }
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

/// How an object names the path `bytes`: as `path`, with U+FFFD where it
/// is not UTF-8, and then as `path_bytes` too, its bytes in hex.
pub fn path(bytes: &[u8]) -> Vec<(&'static str, Json)> {
    let mut fields = vec![("path", Json::str(bytes))];
    if std::str::from_utf8(bytes).is_err() {
        fields.push(("path_bytes", Json::str(crate::machine::hex(bytes))));
    }
    fields
}

/// How an object names several paths: as the list `name` of them, each
/// with U+FFFD where it is not UTF-8, and then as `bytes_name` too, which
/// has for each such path its place in the list and its bytes in hex.
pub fn paths<'a>(
    name: &'static str,
    bytes_name: &'static str,
    paths: impl Iterator<Item = &'a [u8]>,
) -> Vec<(&'static str, Json)> {
    let paths: Vec<&[u8]> = paths.collect();
    let odd = paths.iter().enumerate();
    let odd = odd.filter(|(_, path)| std::str::from_utf8(path).is_err());
    let bytes = |(at, path): (usize, &&[u8])| {
        let hex = Json::str(crate::machine::hex(path));
        Json::Object(vec![("index", Json::Num(at as i64)), ("path_bytes", hex)])
    };
    let bytes: Vec<Json> = odd.map(bytes).collect();
    let mut fields = vec![(name, Json::Array(paths.iter().map(Json::str).collect()))];
    if !bytes.is_empty() {
        fields.push((bytes_name, Json::Array(bytes)));
    }
    fields
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

#[cfg(test)]
mod tests {
    use super::*;

    fn written(fields: Vec<(&'static str, Json)>) -> String {
        let mut out = Vec::new();
        Json::Object(fields).write(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_path_that_is_not_utf8_has_its_bytes_beside_it() {
        assert_eq!(written(path(b"a.txt")), r#"{"path":"a.txt"}"#);
        assert_eq!(
            written(path(b"n\xff")),
            "{\"path\":\"n\u{fffd}\",\"path_bytes\":\"6eff\"}"
        );
        let plain: [&[u8]; 2] = [b"a", b"b"];
        let listed = written(paths("untracked", "untracked_bytes", plain.into_iter()));
        assert_eq!(listed, r#"{"untracked":["a","b"]}"#);
        let odd: [&[u8]; 3] = [b"a", b"\xfe", b"n\xff"];
        let listed = written(paths("untracked", "untracked_bytes", odd.into_iter()));
        let bytes = r#"[{"index":1,"path_bytes":"fe"},{"index":2,"path_bytes":"6eff"}]"#;
        let names = "[\"a\",\"\u{fffd}\",\"n\u{fffd}\"]";
        assert_eq!(
            listed,
            format!("{{\"untracked\":{names},\"untracked_bytes\":{bytes}}}")
        );
    }
}
