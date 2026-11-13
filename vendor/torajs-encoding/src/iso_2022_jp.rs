//! The ISO-2022-JP decoder (§12.2.1): escape sequences switch between
//! ASCII, JIS X 0201 Roman, halfwidth katakana and JIS X 0208 byte
//! pairs.
//!
//! The state, the state to return to after a broken escape, a leading
//! byte and whether the last thing decoded was an escape wait in
//! [`Iso2022JpDecoder`] for the next chunk.

use alloc::vec::Vec;

use crate::cjk_index;
use crate::sink::{HIGH, Sink, bytes_equal};
use crate::{DecodeError, error, push_char};

/// ISO-2022-JP decoder state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
enum State {
    #[default]
    Ascii,
    Roman,
    Katakana,
    LeadingByte,
    TrailingByte,
    EscapeStart,
    Escape,
}

impl State {
    const ALL: [State; 7] = [
        State::Ascii,
        State::Roman,
        State::Katakana,
        State::LeadingByte,
        State::TrailingByte,
        State::EscapeStart,
        State::Escape,
    ];
}

/// The decoder's state between chunks: ISO-2022-JP decoder state,
/// decoder output state, leading and output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Iso2022JpDecoder {
    state: State,
    /// The state a broken escape sequence returns to: ASCII, Roman,
    /// katakana or leading byte.
    output_state: State,
    /// The first byte of a pair in the trailing byte state, of an
    /// escape sequence in the escape state; 0x00 otherwise.
    lead: u8,
    /// The last item decoded was an escape sequence.
    output: bool,
}

impl Iso2022JpDecoder {
    /// The state as four bytes, for a caller that keeps it outside the
    /// decoder: the decoder state and the output state (0 ASCII,
    /// 1 Roman, 2 katakana, 3 leading byte, 4 trailing byte, 5 escape
    /// start, 6 escape), the leading byte, 1 for output else 0.
    pub fn to_bytes(self) -> [u8; 4] {
        [
            self.state as u8,
            self.output_state as u8,
            self.lead,
            u8::from(self.output),
        ]
    }

    /// The state [`Self::to_bytes`] wrote. A state the decoder cannot
    /// be in (an unknown state number, an output state past leading
    /// byte, a leading byte its state cannot have kept) reads as the
    /// initial state.
    pub fn from_bytes(b: [u8; 4]) -> Self {
        let state = State::ALL.get(usize::from(b[0]));
        let output_state = State::ALL[..4].get(usize::from(b[1]));
        let (Some(&state), Some(&output_state)) = (state, output_state) else {
            return Iso2022JpDecoder::default();
        };
        let lead_ok = match state {
            State::TrailingByte => (0x21..=0x7E).contains(&b[2]),
            State::Escape => matches!(b[2], 0x24 | 0x28),
            _ => b[2] == 0,
        };
        if !lead_ok {
            return Iso2022JpDecoder::default();
        }
        Iso2022JpDecoder {
            state,
            output_state,
            lead: b[2],
            output: b[3] != 0,
        }
    }

    /// Decode the chunk `input`, appending UTF-8 to `out`; `last` marks
    /// the end of the queue.
    pub fn decode(
        &mut self,
        input: &[u8],
        last: bool,
        fatal: bool,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        let mut s = Sink::new(out, input.len() + input.len() / 2);
        let mut i = 0;
        loop {
            match self.state {
                // a run of bytes that decode as themselves, in one step
                State::Ascii => {
                    let stop = |w| {
                        (w & HIGH)
                            | bytes_equal(w, 0x0E)
                            | bytes_equal(w, 0x0F)
                            | bytes_equal(w, 0x1B)
                    };
                    let end = s.run(input, i, stop);
                    if end != i {
                        self.output = false;
                        i = end;
                    }
                }
                // jis0208 pairs with an entry, a step each
                State::LeadingByte => {
                    while let Some(&[leading @ 0x21..=0x7E, b @ 0x21..=0x7E]) = input.get(i..i + 2)
                        && let Some(c) = jis0208_pair(leading, b)
                    {
                        s.char(c);
                        self.output = false;
                        i += 2;
                    }
                }
                _ => {}
            }
            let Some(&b) = input.get(i) else {
                break;
            };
            // a whole escape sequence, in one step
            if b == 0x1B
                && (self.state as u8) <= State::LeadingByte as u8
                && let Some(&[_, first, second]) = input.get(i..i + 3)
                && let Some(next) = escape(first, second)
            {
                // two escape sequences in a row are an error
                if core::mem::replace(&mut self.output, true) {
                    s.error(fatal)?;
                }
                self.state = next;
                self.output_state = next;
                i += 3;
                continue;
            }
            i += 1;
            s.with_vec(|out| self.item(Some(b), fatal, out))?;
        }
        // the end of the queue stays in it until the handler finishes
        while last && !s.with_vec(|out| self.item(None, fatal, out))? {}
        Ok(())
    }

    /// The handler's steps for one item, `None` for the end of the
    /// queue; true when finished. An item the standard restores to the
    /// queue is handled again right after the error.
    fn item(&mut self, b: Option<u8>, fatal: bool, out: &mut Vec<u8>) -> Result<bool, DecodeError> {
        match self.state {
            State::Ascii | State::Roman | State::Katakana | State::LeadingByte => {
                let Some(b) = b else {
                    return Ok(true);
                };
                if b == 0x1B {
                    self.state = State::EscapeStart;
                    return Ok(false);
                }
                self.output = false;
                let code_point = match (self.state, b) {
                    (State::Ascii, 0x0E | 0x0F) | (State::Roman, 0x0E | 0x0F) => None,
                    (State::Roman, 0x5C) => Some('\u{A5}'),
                    (State::Roman, 0x7E) => Some('\u{203E}'),
                    (State::Ascii | State::Roman, 0x00..=0x7F) => Some(char::from(b)),
                    (State::Katakana, 0x21..=0x5F) => {
                        // SAFETY: U+FF61 to U+FF9F are scalar values
                        Some(unsafe { char::from_u32_unchecked(0xFF61 - 0x21 + u32::from(b)) })
                    }
                    (State::LeadingByte, 0x21..=0x7E) => {
                        self.lead = b;
                        self.state = State::TrailingByte;
                        return Ok(false);
                    }
                    _ => None,
                };
                match code_point {
                    Some(c) => push_char(out, c),
                    None => error(out, fatal)?,
                }
            }
            State::TrailingByte => {
                let leading = core::mem::take(&mut self.lead);
                if b == Some(0x1B) {
                    self.state = State::EscapeStart;
                    error(out, fatal)?;
                    return Ok(false);
                }
                self.state = State::LeadingByte;
                let pair = b
                    .filter(|b| (0x21..=0x7E).contains(b))
                    .and_then(|b| jis0208_pair(leading, b));
                match pair {
                    Some(c) => push_char(out, c),
                    None => error(out, fatal)?,
                }
            }
            State::EscapeStart => {
                if let Some(b @ (0x24 | 0x28)) = b {
                    self.lead = b;
                    self.state = State::Escape;
                    return Ok(false);
                }
                self.output = false;
                self.state = self.output_state;
                error(out, fatal)?;
                if let Some(b) = b {
                    self.item(Some(b), fatal, out)?;
                }
            }
            State::Escape => {
                let leading = core::mem::take(&mut self.lead);
                let next = b.and_then(|b| escape(leading, b));
                if let Some(next) = next {
                    self.state = next;
                    self.output_state = next;
                    // two escape sequences in a row are an error
                    if core::mem::replace(&mut self.output, true) {
                        error(out, fatal)?;
                    }
                    return Ok(false);
                }
                self.output = false;
                self.state = self.output_state;
                error(out, fatal)?;
                self.item(Some(leading), fatal, out)?;
                if let Some(b) = b {
                    self.item(Some(b), fatal, out)?;
                }
            }
        }
        Ok(false)
    }
}

/// The state the escape sequence 0x1B, `first`, `second` switches to.
#[inline(always)]
fn escape(first: u8, second: u8) -> Option<State> {
    match (first, second) {
        (0x28, 0x42) => Some(State::Ascii),
        (0x28, 0x4A) => Some(State::Roman),
        (0x28, 0x49) => Some(State::Katakana),
        (0x24, 0x40 | 0x42) => Some(State::LeadingByte),
        _ => None,
    }
}

/// The jis0208 code point of the pair `leading`, `b`, both 0x21 to 0x7E.
#[inline(always)]
fn jis0208_pair(leading: u8, b: u8) -> Option<char> {
    cjk_index::jis0208(usize::from(leading - 0x21) * 94 + usize::from(b - 0x21))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn whole(input: &[u8], fatal: bool) -> Result<String, DecodeError> {
        let mut out = Vec::new();
        Iso2022JpDecoder::default().decode(input, true, fatal, &mut out)?;
        Ok(String::from_utf8(out).expect("utf-8 out"))
    }

    #[test]
    fn every_state() {
        // ASCII, JIS X 0208 (日本), Roman, katakana, back to ASCII
        let input = b"a\x1B$BF|K\\\x1B(J\\~\x1B(I!_\x1B(B\\~";
        assert_eq!(
            whole(input, true),
            Ok("a\u{65E5}\u{672C}\u{A5}\u{203E}\u{FF61}\u{FF9F}\\~".into())
        );
        // ESC $ @ is JIS X 0208 too
        assert_eq!(whole(b"\x1B$@F|\x1B(B", true), Ok("\u{65E5}".into()));
    }

    #[test]
    fn errors() {
        // shift out / shift in, a byte past 0x7F
        assert_eq!(
            whole(b"\x0E\x0F\x80", false),
            Ok("\u{FFFD}\u{FFFD}\u{FFFD}".into())
        );
        // two escape sequences in a row
        assert_eq!(whole(b"\x1B(B\x1B(Ja", false), Ok("\u{FFFD}a".into()));
        assert_eq!(whole(b"\x1B(J\x1B(Ba", true), Err(DecodeError));
        // a broken escape restores its bytes in the output state
        assert_eq!(whole(b"\x1B(Z", false), Ok("\u{FFFD}(Z".into()));
        assert_eq!(whole(b"\x1BA", false), Ok("\u{FFFD}A".into()));
        assert_eq!(
            whole(b"\x1B$BF|\x1B(Z", false),
            Ok("\u{65E5}\u{FFFD}\u{FFFD}".into())
        );
        // an escape before the trailing byte, and a pair without an entry
        assert_eq!(whole(b"\x1B$BF\x1B(Ba", false), Ok("\u{FFFD}a".into()));
        assert_eq!(
            whole(b"\x1B$B\x7F\x29\x21", false),
            Ok("\u{FFFD}\u{FFFD}".into())
        );
        // the end of the queue after an escape start, an escape, a leading byte
        assert_eq!(whole(b"a\x1B", false), Ok("a\u{FFFD}".into()));
        assert_eq!(whole(b"a\x1B$", false), Ok("a\u{FFFD}$".into()));
        assert_eq!(whole(b"\x1B$B\x1B$", false), Ok("\u{FFFD}\u{FFFD}".into()));
        assert_eq!(whole(b"\x1B$BF", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\x1B$BF", true), Err(DecodeError));
        assert_eq!(whole(b"\x1B$B", true), Ok("".into()));
    }

    #[test]
    fn a_chunk_boundary_splits_nothing() {
        let input = b"\x1B$BF|K\\\x1B(I1\x1B(Bz";
        for cut in 0..=input.len() {
            let mut d = Iso2022JpDecoder::default();
            let mut out = Vec::new();
            d.decode(&input[..cut], false, true, &mut out)
                .expect("decodes");
            let mut d = Iso2022JpDecoder::from_bytes(d.to_bytes());
            d.decode(&input[cut..], true, true, &mut out)
                .expect("decodes");
            assert_eq!(out, "\u{65E5}\u{672C}\u{FF71}z".as_bytes(), "cut at {cut}");
        }
    }

    #[test]
    fn from_bytes_keeps_only_a_state_the_decoder_can_be_in() {
        let d = Iso2022JpDecoder::from_bytes([4, 3, 0x46, 1]);
        assert_eq!(d.to_bytes(), [4, 3, 0x46, 1]);
        for b in [
            [7, 0, 0, 0],
            [0, 4, 0, 0],
            [4, 3, 0x7F, 0],
            [6, 0, 0x29, 0],
            [0, 0, 1, 0],
        ] {
            assert_eq!(
                Iso2022JpDecoder::from_bytes(b),
                Iso2022JpDecoder::default(),
                "{b:?}"
            );
        }
    }
}
