//! §4.2 "get an encoding": a label to its encoding.

use crate::encoding::{Encoding, LABEL_BYTES, LABEL_DISPLACEMENTS, LABEL_HASH, LABEL_SLOTS};

/// No label is longer.
const LONGEST: usize = 20;

impl Encoding {
    /// The encoding `label` names: leading and trailing ASCII whitespace
    /// removed, then matched ASCII case-insensitively against the
    /// standard's labels. `None` is the standard's failure.
    pub fn for_label(label: &[u8]) -> Option<Encoding> {
        let ws = |b: &u8| matches!(b, b'\t' | b'\n' | b'\x0C' | b'\r' | b' ');
        let start = label.iter().position(|b| !ws(b))?;
        let end = label.iter().rposition(|b| !ws(b)).map_or(start, |i| i + 1);
        let label = &label[start..end];
        if label.len() > LONGEST {
            return None;
        }
        let raw = key_words(label);
        // bit 5 set in every byte: the hash ignores ASCII case, so the
        // lowercasing below is off its path
        let folded = raw.map(|w| w | 0x2020_2020_2020_2020);
        let h = (folded[0].wrapping_mul(LABEL_HASH[0]))
            ^ (folded[1].wrapping_mul(LABEL_HASH[1]))
            ^ (folded[2].wrapping_mul(LABEL_HASH[2]));
        let w = raw.map(ascii_lowercase);
        let d = LABEL_DISPLACEMENTS[(h >> 57) as usize];
        let (at, len, encoding) = LABEL_SLOTS[usize::from((h >> 32) as u8 ^ d)];
        // a key with a zero byte pads like a shorter label, so the
        // lengths must agree too; an empty slot's length matches no key
        if usize::from(len) != label.len() {
            return None;
        }
        let at = usize::from(at);
        // the key is zero past its length, the stored bytes run on into
        // the next label: compare only the key's bytes
        let mut differ = 0;
        for (i, &kw) in w.iter().enumerate() {
            let have = label.len().saturating_sub(8 * i).min(8);
            let mask = if have == 8 {
                u64::MAX
            } else {
                (1u64 << (8 * have)) - 1
            };
            differ |= (word(LABEL_BYTES, at + 8 * i) ^ kw) & mask;
        }
        (differ == 0).then_some(encoding)
    }
}

/// The eight bytes of `bytes` from `at` on, little-endian.
#[inline(always)]
fn word(bytes: &[u8], at: usize) -> u64 {
    let mut w = [0; 8];
    w.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(w)
}

/// `key`, at most [`LONGEST`] bytes, as three little-endian words, zero
/// past its end. Each length reads with loads that overlap inside the
/// key; an overlapped byte reads the same either way.
#[inline(always)]
fn key_words(key: &[u8]) -> [u64; 3] {
    let n = key.len();
    let half = |at: usize| {
        let mut w = [0; 4];
        w.copy_from_slice(&key[at..at + 4]);
        u64::from(u32::from_le_bytes(w))
    };
    match n {
        0 => [0; 3],
        1..=3 => {
            let byte = |at: usize| u64::from(key[at]) << (8 * at);
            [byte(0) | byte(n / 2) | byte(n - 1), 0, 0]
        }
        4..=7 => [half(0) | half(n - 4) << (8 * (n - 4)), 0, 0],
        8 => [word(key, 0), 0, 0],
        9..=16 => [word(key, 0), word(key, n - 8) >> (8 * (16 - n)), 0],
        _ => [word(key, 0), word(key, 8), half(n - 4) >> (8 * (20 - n))],
    }
}

/// `w` with every ASCII upper case letter byte made lower case.
#[inline(always)]
fn ascii_lowercase(w: u64) -> u64 {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let low = w & !HIGH;
    // a byte's high bit: its low seven bits are at least b'A', at
    // least b'Z' + 1; no byte carries into the next
    let from_a = low + 0x3F3F_3F3F_3F3F_3F3F;
    let past_z = low + 0x2525_2525_2525_2525;
    let upper = from_a & !past_z & !w & HIGH;
    w | (upper >> 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    fn stored_labels() -> Vec<(String, Encoding)> {
        (LABEL_SLOTS.iter())
            .filter(|&&(_, len, _)| len != u8::MAX)
            .map(|&(at, len, e)| {
                let bytes = &LABEL_BYTES[usize::from(at)..][..usize::from(len)];
                (String::from_utf8(bytes.into()).expect("ascii label"), e)
            })
            .collect()
    }

    #[test]
    fn every_label_finds_its_encoding() {
        let labels = stored_labels();
        assert_eq!(labels.len(), 228);
        assert!(labels.iter().all(|(l, _)| l.len() <= LONGEST));
        for (l, e) in &labels {
            assert_eq!(Encoding::for_label(l.as_bytes()), Some(*e), "{l}");
            let upper = l.to_ascii_uppercase();
            assert_eq!(Encoding::for_label(upper.as_bytes()), Some(*e), "{upper}");
            // every proper prefix and every one-byte extension misses
            // unless it is itself a label
            for cut in 0..l.len() {
                let p = &l[..cut];
                let want = labels.iter().find(|(m, _)| m == p).map(|&(_, e)| e);
                assert_eq!(Encoding::for_label(p.as_bytes()), want, "{p:?}");
            }
            for extra in [b'\0', b'x', b'-', 0x80] {
                let mut longer = Vec::from(l.as_bytes());
                longer.push(extra);
                let want = labels.iter().find(|(m, _)| m.as_bytes() == longer);
                assert_eq!(
                    Encoding::for_label(&longer),
                    want.map(|&(_, e)| e),
                    "{longer:?}"
                );
            }
        }
    }

    #[test]
    fn labels_trim_ascii_whitespace_and_fold_ascii_case() {
        let cases = [
            ("latin1", Encoding::Windows1252),
            (" \t\n\x0C\rISO-8859-1\r", Encoding::Windows1252),
            ("UTF-16", Encoding::Utf16le),
            ("unicodefffe", Encoding::Utf16be),
            ("csiso2022kr", Encoding::Replacement),
            ("x-sjis", Encoding::ShiftJis),
            ("Utf8", Encoding::Utf8),
            ("  cseucpkdfmtjapanese  ", Encoding::EucJp),
        ];
        for (l, e) in cases {
            assert_eq!(Encoding::for_label(l.as_bytes()), Some(e), "{l:?}");
        }
        let fails = [
            "",
            " ",
            "foo",
            "utf 8",
            "\u{a0}utf-8",
            "utf-8\0",
            "\u{212a}oi8-r",
            "\x0Butf-8",
            "cseucpkdfmtjapanesex",
            "cseucpkdfmtjapanese-x",
        ];
        for l in fails {
            assert_eq!(Encoding::for_label(l.as_bytes()), None, "{l:?}");
        }
    }

    #[test]
    fn key_words_read_every_length_and_fold_only_ascii_letters() {
        let pattern = [b'A', b'z', b'@', b'[', 0xC1, b'Q', b'-'];
        let bytes: Vec<u8> = (0..20u8)
            .map(|i| pattern[usize::from(i % 7)] ^ (i & 0x10))
            .collect();
        for n in 0..=LONGEST {
            let mut want = [0u8; 24];
            for (w, b) in want.iter_mut().zip(&bytes[..n]) {
                *w = b.to_ascii_lowercase();
            }
            let got = key_words(&bytes[..n]).map(ascii_lowercase);
            let want = [word(&want, 0), word(&want, 8), word(&want, 16)];
            assert_eq!(got, want, "length {n}");
        }
        for b in 0..=255u8 {
            let mut w = [b; 8];
            w.make_ascii_lowercase();
            assert_eq!(
                ascii_lowercase(u64::from_le_bytes([b; 8])),
                u64::from_le_bytes(w)
            );
        }
    }

    #[test]
    fn names_and_discriminants_follow_the_table() {
        for (i, e) in Encoding::ALL.iter().enumerate() {
            assert_eq!(*e as usize, i);
        }
        assert_eq!(Encoding::Utf8.name(), "UTF-8");
        assert_eq!(Encoding::Iso8859_8I.name(), "ISO-8859-8-I");
        assert_eq!(Encoding::XUserDefined.name(), "x-user-defined");
    }
}
