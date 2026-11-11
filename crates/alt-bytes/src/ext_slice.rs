use std::{borrow::Cow, ffi::OsStr, path::Path, str::Utf8Error};

use crate::BStr;
use crate::block::{self, BLOCK};

/// String-like operations on `[u8]`.
pub trait ByteSlice {
    /// Returns the receiver as a plain byte slice.
    fn as_bytes(&self) -> &[u8];

    /// Views the bytes as a [`BStr`], for printing them as text.
    fn as_bstr(&self) -> &BStr {
        BStr::new(self.as_bytes())
    }

    /// Returns the bytes as a `&str` if they are valid UTF-8.
    fn to_str(&self) -> Result<&str, Utf8Error> {
        std::str::from_utf8(self.as_bytes())
    }

    /// Converts the bytes to text, replacing each maximal invalid UTF-8
    /// sequence with U+FFFD.
    fn to_str_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(self.as_bytes())
    }

    /// Returns the bytes as an OS string. On Unix this never fails; on
    /// other platforms the bytes must be valid UTF-8.
    fn to_os_str(&self) -> Result<&OsStr, Utf8Error> {
        #[cfg(unix)]
        {
            Ok(std::os::unix::ffi::OsStrExt::from_bytes(self.as_bytes()))
        }
        #[cfg(not(unix))]
        {
            self.to_str().map(OsStr::new)
        }
    }

    /// Returns the bytes as a path, under the same rule as
    /// [`ByteSlice::to_os_str`].
    fn to_path(&self) -> Result<&Path, Utf8Error> {
        self.to_os_str().map(Path::new)
    }

    /// Removes Unicode whitespace from both ends. Bytes that are not valid
    /// UTF-8 are never whitespace.
    fn trim(&self) -> &[u8] {
        let mut bytes = self.trim_end();
        while let Some(ch) = first_char(bytes).filter(|ch| ch.is_whitespace()) {
            bytes = &bytes[ch.len_utf8()..];
        }
        bytes
    }

    /// Removes Unicode whitespace from the end. Bytes that are not valid
    /// UTF-8 are never whitespace.
    fn trim_end(&self) -> &[u8] {
        let mut bytes = self.as_bytes();
        while let Some(ch) = last_char(bytes).filter(|ch| ch.is_whitespace()) {
            bytes = &bytes[..bytes.len() - ch.len_utf8()];
        }
        bytes
    }

    /// Iterates over the lines, without their `\n` or `\r\n` terminator.
    /// A final line needs no terminator, and an empty input has no lines.
    fn lines(&self) -> Lines<'_> {
        Lines {
            rest: self.as_bytes(),
        }
    }

    /// Returns the index of the first occurrence of `needle`. An empty
    /// needle is found at index 0.
    fn find<B: AsRef<[u8]>>(&self, needle: B) -> Option<usize> {
        let haystack = self.as_bytes();
        let Some((&first, rest)) = needle.as_ref().split_first() else {
            return Some(0);
        };
        let mut from = 0;
        while let Some(skipped) = haystack[from..].find_byte(first) {
            let at = from + skipped;
            if haystack[at + 1..].starts_with(rest) {
                return Some(at);
            }
            from = at + 1;
        }
        None
    }

    /// Returns the index of the first occurrence of `byte`.
    fn find_byte(&self, byte: u8) -> Option<usize> {
        let (blocks, tail) = self.as_bytes().as_chunks::<BLOCK>();
        for (i, block) in blocks.iter().enumerate() {
            if let Some(at) = block::first(block, byte) {
                return Some(i * BLOCK + at);
            }
        }
        let found = tail.iter().position(|&b| b == byte)?;
        Some(blocks.len() * BLOCK + found)
    }

    /// Returns the index of the last occurrence of `byte`.
    fn rfind_byte(&self, byte: u8) -> Option<usize> {
        let (head, blocks) = self.as_bytes().as_rchunks::<BLOCK>();
        for (i, block) in blocks.iter().enumerate().rev() {
            if let Some(at) = block::last(block, byte) {
                return Some(head.len() + i * BLOCK + at);
            }
        }
        head.iter().rposition(|&b| b == byte)
    }
}

impl ByteSlice for [u8] {
    fn as_bytes(&self) -> &[u8] {
        self
    }
}

/// The iterator returned by [`ByteSlice::lines`].
pub struct Lines<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Lines<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.rest.is_empty() {
            return None;
        }
        let Some(newline) = self.rest.find_byte(b'\n') else {
            return Some(std::mem::take(&mut self.rest));
        };
        let line = &self.rest[..newline];
        self.rest = &self.rest[newline + 1..];
        Some(line.strip_suffix(b"\r").unwrap_or(line))
    }
}

fn first_char(bytes: &[u8]) -> Option<char> {
    let head = &bytes[..bytes.len().min(4)];
    head.utf8_chunks().next()?.valid().chars().next()
}

fn last_char(bytes: &[u8]) -> Option<char> {
    // the shortest valid suffix is exactly one character
    (1..=bytes.len().min(4))
        .find_map(|width| std::str::from_utf8(&bytes[bytes.len() - width..]).ok())?
        .chars()
        .next()
}
