//! The shape the Big5 (§11.1.1), Shift_JIS (§12.3.1) and EUC-KR
//! (§13.1.1) decoders share: a leading byte waits for the byte after
//! it, an ASCII byte that cannot follow it goes back to the queue and
//! comes out as itself, and a leading byte left at the end of the queue
//! is one error.

use alloc::vec::Vec;

use crate::DecodeError;
use crate::sink::{LONE, Sink};

/// What the handler does with a byte that does not lead while no
/// leading byte waits.
pub(crate) enum Single {
    /// Return this code point.
    Char(char),
    /// Return error.
    Error,
}

/// The code points a leading byte and the byte after it decode to: one,
/// or for Big5's four pointers two; `None` is the handler's error.
pub(crate) type Pair = Option<(char, Option<char>)>;

/// The per-byte answers of one decoder of this shape.
pub(crate) trait Bytes {
    /// `b` is a leading byte.
    fn is_lead(b: u8) -> bool;
    /// What `b`, a byte that does not lead, decodes to without a
    /// leading byte before it.
    fn single(b: u8) -> Single;
    /// What the leading byte `leading` and the byte `b` decode to.
    fn pair(leading: u8, b: u8) -> Pair;
}

/// Decode `input` as `B` answers, with the leading byte `lead` (0 for
/// none) carried in and out. Every ASCII byte without a leading byte
/// before it decodes as itself.
#[inline(always)]
pub(crate) fn decode<B: Bytes>(
    lead: &mut u8,
    input: &[u8],
    last: bool,
    fatal: bool,
    out: &mut Vec<u8>,
) -> Result<(), DecodeError> {
    let (is_lead, single, pair) = (B::is_lead, B::single, B::pair);
    let mut s = Sink::new(out, input.len() + input.len() / 2);
    let mut i = 0;
    if *lead != 0
        && let Some(&b) = input.first()
    {
        finish(&mut s, pair(core::mem::take(lead), b), b, fatal)?;
        i = 1;
    }
    loop {
        if let Some(&[b, next]) = input.get(i..i + 2)
            && is_lead(b)
        {
            i += 2;
            finish(&mut s, pair(b, next), next, fatal)?;
            continue;
        }
        let Some(&b) = input.get(i) else {
            break;
        };
        if is_lead(b) {
            *lead = b;
            break;
        }
        if b.is_ascii() {
            i = s.ascii_from::<LONE>(input, i, b);
            continue;
        }
        i += 1;
        match single(b) {
            Single::Char(c) => s.char(c),
            Single::Error => s.error(fatal)?,
        }
    }
    if last && *lead != 0 {
        *lead = 0;
        s.error(fatal)?;
    }
    Ok(())
}

/// Write what a leading byte and the byte `b` after it decoded to.
#[inline(always)]
fn finish(s: &mut Sink<'_>, decoded: Pair, b: u8, fatal: bool) -> Result<(), DecodeError> {
    match decoded {
        Some((c, second)) => {
            s.char(c);
            if let Some(c) = second {
                s.char(c);
            }
        }
        None => {
            s.error(fatal)?;
            // restored, an ASCII byte decodes as itself
            if b.is_ascii() {
                s.ascii(b);
            }
        }
    }
    Ok(())
}

/// Each byte's column as a trailing byte: its offset from the first
/// byte of the range it is in, among `ranges`, plus the length of the
/// ranges before; [`NO_COLUMN`] where it cannot trail.
pub(crate) const fn columns(ranges: &[(u8, u8)]) -> [u8; 256] {
    let mut table = [NO_COLUMN; 256];
    let (mut r, mut column) = (0, 0u8);
    while r < ranges.len() {
        let (first, last) = ranges[r];
        let mut b = first as usize;
        while b <= last as usize {
            table[b] = column;
            column += 1;
            b += 1;
        }
        r += 1;
    }
    table
}

/// A byte that cannot trail, in a [`columns`] table.
pub(crate) const NO_COLUMN: u8 = 0xFF;

/// A decoder whose whole state is one leading byte.
macro_rules! lead_decoder {
    ($(#[$doc:meta])* $name:ident, $is_lead:path, $single:path, $pair:path) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct $name {
            /// The leading byte, 0x00 when unset.
            lead: u8,
        }

        impl $name {
            /// The state as four bytes, for a caller that keeps it
            /// outside the decoder: the leading byte (0 for none), then
            /// three zeros.
            pub fn to_bytes(self) -> [u8; 4] {
                [self.lead, 0, 0, 0]
            }

            /// The state [`Self::to_bytes`] wrote. A first byte that
            /// cannot lead reads as none, and the other three are
            /// ignored.
            pub fn from_bytes(b: [u8; 4]) -> Self {
                $name {
                    lead: if <$name as crate::lead::Bytes>::is_lead(b[0]) { b[0] } else { 0 },
                }
            }

            /// Decode the chunk `input`, appending UTF-8 to `out`;
            /// `last` marks the end of the queue.
            pub fn decode(
                &mut self,
                input: &[u8],
                last: bool,
                fatal: bool,
                out: &mut Vec<u8>,
            ) -> Result<(), DecodeError> {
                crate::lead::decode::<$name>(&mut self.lead, input, last, fatal, out)
            }
        }

        impl crate::lead::Bytes for $name {
            #[inline(always)]
            fn is_lead(b: u8) -> bool {
                $is_lead(b)
            }

            #[inline(always)]
            fn single(b: u8) -> crate::lead::Single {
                $single(b)
            }

            #[inline(always)]
            fn pair(leading: u8, b: u8) -> crate::lead::Pair {
                $pair(leading, b)
            }
        }
    };
}

pub(crate) use lead_decoder;
