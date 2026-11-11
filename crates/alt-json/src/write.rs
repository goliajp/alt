//! Compact and indented output.

use crate::digits::{write_i64, write_u64};
use crate::number::N;
use crate::scan::find_special;
use crate::{Number, Value};

const INDENT: &[u8] = b"  ";

pub(crate) fn write_compact(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_compact(item, out);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            out.push(b'{');
            for (i, (key, member)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_string(key, out);
                out.push(b':');
                write_compact(member, out);
            }
            out.push(b'}');
        }
        scalar => write_scalar(scalar, out),
    }
}

/// `depth` is the nesting level of `value` itself.
pub(crate) fn write_pretty(value: &Value, depth: usize, out: &mut Vec<u8>) {
    match value {
        Value::Array(items) if !items.is_empty() => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                new_line(depth + 1, out);
                write_pretty(item, depth + 1, out);
            }
            new_line(depth, out);
            out.push(b']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push(b'{');
            for (i, (key, member)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                new_line(depth + 1, out);
                write_string(key, out);
                out.extend_from_slice(b": ");
                write_pretty(member, depth + 1, out);
            }
            new_line(depth, out);
            out.push(b'}');
        }
        other => write_compact(other, out),
    }
}

fn new_line(depth: usize, out: &mut Vec<u8>) {
    out.push(b'\n');
    for _ in 0..depth {
        out.extend_from_slice(INDENT);
    }
}

fn write_scalar(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(number) => write_number(number, out),
        Value::String(string) => write_string(string, out),
        Value::Array(_) => out.extend_from_slice(b"[]"),
        Value::Object(_) => out.extend_from_slice(b"{}"),
    }
}

pub(crate) fn write_number(number: &Number, out: &mut Vec<u8>) {
    match number.kind() {
        N::PosInt(n) => write_u64(n, out),
        N::NegInt(n) => write_i64(n, out),
        N::Float(n) => crate::float::write_f64(n, out),
    }
}

/// Quotes `string`. Escaped: `"`, `\`, and every byte below 0x20 (as `\b`,
/// `\t`, `\n`, `\f`, `\r` where those exist, otherwise `\u00xx` with
/// lowercase hex). Everything else, including 0x7f and all non-ascii text,
/// is copied as is.
pub(crate) fn write_string(string: &str, out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = string.as_bytes();
    out.push(b'"');
    let mut start = 0;
    loop {
        let stop = find_special(bytes, start);
        out.extend_from_slice(&bytes[start..stop]);
        let Some(&byte) = bytes.get(stop) else {
            break;
        };
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\r' => out.extend_from_slice(b"\\r"),
            _ => out.extend_from_slice(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[usize::from(byte >> 4)],
                HEX[usize::from(byte & 0xf)],
            ]),
        }
        start = stop + 1;
    }
    out.push(b'"');
}
