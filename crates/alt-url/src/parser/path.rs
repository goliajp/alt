use super::Parser;
use crate::percent::{self, EncodeSet};

pub(super) fn is_drive_letter(segment: &str) -> bool {
    starts_with_drive(segment.as_bytes()) && segment.len() == 2
}

fn starts_with_drive(bytes: &[u8]) -> bool {
    matches!(bytes, [letter, b':' | b'|', ..] if letter.is_ascii_alphabetic())
}

pub(super) fn starts_with_drive_letter(input: &str) -> bool {
    let bytes = input.as_bytes();
    starts_with_drive(bytes) && matches!(bytes.get(2), None | Some(b'/' | b'\\' | b'?' | b'#'))
}

pub(super) fn is_normalized_drive_letter(segment: &str) -> bool {
    matches!(segment.as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic())
}

/// 1 for a single-dot segment, 2 for a double-dot segment, 0 otherwise.
fn dots(segment: &str) -> u8 {
    if !matches!(segment.as_bytes().first(), Some(b'.' | b'%')) {
        return 0;
    }
    if segment == "." || segment.eq_ignore_ascii_case("%2e") {
        1
    } else if segment == ".."
        || segment.eq_ignore_ascii_case(".%2e")
        || segment.eq_ignore_ascii_case("%2e.")
        || segment.eq_ignore_ascii_case("%2e%2e")
    {
        2
    } else {
        0
    }
}

impl Parser {
    /// Starts the path of a URL with an authority at `input`.
    pub(super) fn path_start<'i>(&mut self, input: &'i str) -> &'i str {
        self.path_start = self.out.len();
        match input.as_bytes().first() {
            Some(&byte) if self.is_slash(byte) => self.path(&input[1..]),
            None | Some(b'?' | b'#') if !self.special => input,
            _ => self.path(input),
        }
    }

    /// Appends the segments in `input` to the path and returns what follows the path.
    pub(super) fn path<'i>(&mut self, input: &'i str) -> &'i str {
        let bytes = input.as_bytes();
        let mut start = 0;
        loop {
            let end = bytes[start..]
                .iter()
                .position(|&b| matches!(b, b'?' | b'#') || self.is_slash(b))
                .map_or(bytes.len(), |i| start + i);
            let segment = &input[start..end];
            let slash = bytes.get(end).is_some_and(|&b| self.is_slash(b));
            match dots(segment) {
                0 => {
                    self.out.push('/');
                    if self.file
                        && self.out.len() == self.path_start + 1
                        && is_drive_letter(segment)
                    {
                        self.out.push_str(&segment[..1]);
                        self.out.push(':');
                    } else {
                        percent::encode(&mut self.out, segment, EncodeSet::PATH);
                    }
                }
                dots => {
                    if dots == 2 {
                        self.shorten_path();
                    }
                    if !slash {
                        self.out.push('/');
                    }
                }
            }
            if !slash {
                self.end_path();
                return &input[end..];
            }
            start = end + 1;
        }
    }

    pub(super) fn shorten_path(&mut self) {
        let path = &self.out[self.path_start..];
        if self.file && path.len() == 3 && is_normalized_drive_letter(&path[1..]) {
            return;
        }
        if let Some(slash) = path.rfind('/') {
            self.out.truncate(self.path_start + slash);
        }
    }

    /// Keeps a path that starts with `//` from reading as an authority.
    pub(super) fn end_path(&mut self) {
        if !self.authority && self.out[self.path_start..].starts_with("//") {
            self.out.insert_str(self.path_start, "/.");
            self.path_start += 2;
        }
    }
}
