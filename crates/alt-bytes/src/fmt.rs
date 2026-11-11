use std::fmt::{self, Alignment, Debug, Display, Formatter, Write};

use crate::{BStr, BString};

/// Writes the bytes as text, with each maximal invalid UTF-8 sequence shown
/// as U+FFFD.
///
/// Width applies only when an alignment is given explicitly, and counts
/// each invalid sequence as one character. Precision is ignored.
impl Display for BStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Some(align) = f.align() else {
            return write_lossy(f, self);
        };
        let chars: usize = self
            .utf8_chunks()
            .map(|chunk| chunk.valid().chars().count() + usize::from(!chunk.invalid().is_empty()))
            .sum();
        let pads = f.width().unwrap_or(0).saturating_sub(chars);
        let before = match align {
            Alignment::Left => 0,
            Alignment::Right => pads,
            Alignment::Center => pads / 2,
        };
        write_fill(f, before)?;
        write_lossy(f, self)?;
        write_fill(f, pads - before)
    }
}

/// Writes the bytes as a double-quoted string literal.
///
/// Valid UTF-8 is escaped the way `str` is, except that NUL is `\0`, a
/// single quote is `\'`, and other ASCII control bytes are `\xNN`. Bytes
/// that are not valid UTF-8 are `\xNN` as well, which keeps them apart from
/// a U+FFFD that is really in the data.
impl Debug for BStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_char('"')?;
        for chunk in self.utf8_chunks() {
            for ch in chunk.valid().chars() {
                match ch {
                    '\0' => f.write_str("\\0")?,
                    '\x01'..='\x7f' => write!(f, "{}", (ch as u8).escape_ascii())?,
                    _ => write!(f, "{}", ch.escape_debug())?,
                }
            }
            for byte in chunk.invalid() {
                write!(f, "\\x{byte:02x}")?;
            }
        }
        f.write_char('"')
    }
}

impl Display for BString {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(BStr::new(self), f)
    }
}

impl Debug for BString {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(BStr::new(self), f)
    }
}

fn write_lossy(f: &mut Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for chunk in bytes.utf8_chunks() {
        f.write_str(chunk.valid())?;
        if !chunk.invalid().is_empty() {
            f.write_char(char::REPLACEMENT_CHARACTER)?;
        }
    }
    Ok(())
}

fn write_fill(f: &mut Formatter<'_>, count: usize) -> fmt::Result {
    let fill = f.fill();
    (0..count).try_for_each(|_| f.write_char(fill))
}
