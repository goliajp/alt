use std::io::Write;

/// git's `quote_c_style` for path output: if the name contains a quote,
/// backslash, control byte — or any non-ASCII byte while `core.quotePath`
/// is on (the default) — it is wrapped in double quotes with C escapes;
/// otherwise it is written verbatim.
pub fn write_path(out: &mut impl Write, name: &[u8], quotepath: bool) -> std::io::Result<()> {
    let needs_escape =
        |b: u8| b == b'"' || b == b'\\' || b < 0x20 || b == 0x7f || (quotepath && b >= 0x80);
    if !name.iter().any(|&b| needs_escape(b)) {
        return out.write_all(name);
    }
    out.write_all(b"\"")?;
    for &b in name {
        match b {
            b'\x07' => out.write_all(b"\\a")?,
            b'\x08' => out.write_all(b"\\b")?,
            b'\t' => out.write_all(b"\\t")?,
            b'\n' => out.write_all(b"\\n")?,
            b'\x0b' => out.write_all(b"\\v")?,
            b'\x0c' => out.write_all(b"\\f")?,
            b'\r' => out.write_all(b"\\r")?,
            b'"' => out.write_all(b"\\\"")?,
            b'\\' => out.write_all(b"\\\\")?,
            _ if needs_escape(b) => write!(out, "\\{b:03o}")?,
            _ => out.write_all(&[b])?,
        }
    }
    out.write_all(b"\"")
}
