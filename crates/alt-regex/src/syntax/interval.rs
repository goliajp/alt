//! The counts of a repetition written between braces.

use super::Parser;
use crate::Error;

/// The largest repetition count, as in the C library.
const DUP_MAX: u32 = 0x7fff;

impl Parser<'_> {
    fn number(&mut self) -> Result<Option<u32>, Error> {
        let mut value = None;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            let next = value.unwrap_or(0) * 10 + u32::from(digit - b'0');
            if next > DUP_MAX {
                return Err(Error::TooBig);
            }
            value = Some(next);
            self.at += 1;
        }
        Ok(value)
    }

    /// The bounds of `{m}`, `{m,}`, `{m,n}` or `{,n}`, behind the brace.
    pub(super) fn interval(&mut self) -> Result<(u32, Option<u32>), Error> {
        let min = self.number()?;
        let (min, max) = match (min, self.peek()) {
            (_, Some(b',')) => {
                self.at += 1;
                (min.unwrap_or(0), self.number()?)
            }
            (Some(min), _) => (min, Some(min)),
            (None, _) => return Err(Error::Brace),
        };
        let close: &[u8] = if self.ere() { b"}" } else { b"\\}" };
        if !self.rest().starts_with(close) {
            return Err(Error::Brace);
        }
        self.at += close.len();
        if max.is_some_and(|max| max < min) {
            return Err(Error::Brace);
        }
        Ok((min, max))
    }
}
