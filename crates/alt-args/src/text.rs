//! How a description is written as text.
//!
//! A number is one, two or three bytes below 128, the first the most
//! significant; a string is its length as such a number and then its
//! bytes. Every byte that is not part of a string is ASCII, so the whole
//! is a `str` and a string is cut out of it without a check of its own.

/// A position in the text of a description.
#[derive(Clone, Copy)]
pub(crate) struct Reader {
    text: &'static str,
    at: usize,
}

impl Reader {
    pub(crate) fn new(text: &'static str, at: usize) -> Reader {
        Reader { text, at }
    }

    pub(crate) fn one(&mut self) -> usize {
        let byte = self.text.as_bytes()[self.at];
        self.at += 1;
        usize::from(byte)
    }

    pub(crate) fn two(&mut self) -> usize {
        self.one() * 128 + self.one()
    }

    pub(crate) fn three(&mut self) -> usize {
        self.two() * 128 + self.one()
    }

    fn take(&mut self, length: usize) -> &'static str {
        let taken = &self.text[self.at..self.at + length];
        self.at += length;
        taken
    }

    /// A string whose length is one byte.
    pub(crate) fn short(&mut self) -> &'static str {
        let length = self.one();
        self.take(length)
    }

    /// A string whose length is two bytes.
    pub(crate) fn long(&mut self) -> &'static str {
        let length = self.two();
        self.take(length)
    }

    pub(crate) fn skip(&mut self, length: usize) {
        self.at += length;
    }

    /// What is left of the text.
    #[cfg(test)]
    fn rest(&self) -> &'static str {
        &self.text[self.at..]
    }
}

/// The longest string a two-byte length can announce.
const LONGEST: usize = 128 * 128 - 1;

/// The length of [`joined`] for `pieces`.
pub const fn joined_len(pieces: &[&str]) -> usize {
    let mut length = 0;
    let mut at = 0;
    while at < pieces.len() {
        length += 2 + pieces[at].len();
        at += 1;
    }
    length
}

/// `pieces` one after the other, each a string with a two-byte length.
/// The derive macros join what only the compiler knows, the accepted
/// values of the types of a command's arguments, into one text.
pub const fn joined<const N: usize>(pieces: &[&str]) -> [u8; N] {
    let mut out = [0; N];
    let mut to = 0;
    let mut at = 0;
    while at < pieces.len() {
        let piece = pieces[at].as_bytes();
        assert!(
            piece.len() <= LONGEST,
            "a list of accepted values is too long"
        );
        out[to] = (piece.len() / 128) as u8;
        out[to + 1] = (piece.len() % 128) as u8;
        to += 2;
        let mut byte = 0;
        while byte < piece.len() {
            out[to] = piece[byte];
            to += 1;
            byte += 1;
        }
        at += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_are_joined_with_their_lengths_and_read_back() {
        const PIECES: &[&str] = &["true\nfalse", "", "né"];
        const LENGTH: usize = joined_len(PIECES);
        const BYTES: [u8; LENGTH] = joined(PIECES);
        let text: &'static str = match std::str::from_utf8(&BYTES) {
            Ok(text) => text,
            Err(_) => panic!("not text"),
        };
        let mut reader = Reader::new(text, 0);
        assert_eq!(reader.long(), "true\nfalse");
        assert_eq!(reader.long(), "");
        assert_eq!(reader.long(), "né");
        assert_eq!(reader.rest(), "");
    }

    #[test]
    fn numbers_take_the_most_significant_byte_first() {
        let mut reader = Reader::new("\u{1}\u{2}\u{1}\u{0}\u{3}\u{1}a", 0);
        assert_eq!(reader.two(), 130);
        assert_eq!(reader.three(), 128 * 128 + 3);
        assert_eq!(reader.short(), "a");
    }
}
