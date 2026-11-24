//! `core.precomposeUnicode`: a file system of macOS may list a name
//! with its letters taken apart, and git takes every name it reads there
//! put together again. It does so by converting the name from `UTF-8-MAC`
//! to `UTF-8`, which is not Normalization Form C: only a code point and
//! the one right after it are put together, where the decomposition of
//! HFS+ takes them apart, and nothing is reordered or replaced. A tree
//! must hold the bytes git records, so this is that conversion.

mod pairs;

use pairs::PAIRS;

const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const S_BASE: u32 = 0xAC00;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;

/// The code point `first` and `second` are put together into.
fn pair(first: u32, second: u32) -> Option<u32> {
    // a leading consonant and a vowel, or such a syllable and a
    // trailing consonant
    if (L_BASE..L_BASE + L_COUNT).contains(&first) && (V_BASE..V_BASE + V_COUNT).contains(&second) {
        return Some(S_BASE + ((first - L_BASE) * V_COUNT + (second - V_BASE)) * T_COUNT);
    }
    let syllable = (S_BASE..S_BASE + L_COUNT * V_COUNT * T_COUNT).contains(&first);
    if syllable
        && (first - S_BASE).is_multiple_of(T_COUNT)
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&second)
    {
        return Some(first + (second - T_BASE));
    }
    let (first, second) = (u16::try_from(first).ok()?, u16::try_from(second).ok()?);
    let at = PAIRS.binary_search_by_key(&(first, second), |(a, b, _)| (*a, *b));
    at.ok().map(|at| u32::from(PAIRS[at].2))
}

/// Every pair this puts together but the Hangul syllables: the first,
/// the second, and what they become. For a check against the decoder.
pub fn pairs() -> impl Iterator<Item = [char; 3]> {
    let known = |point: u16| char::from_u32(u32::from(point)).expect("no surrogate is in it");
    PAIRS
        .iter()
        .map(move |(a, b, c)| [known(*a), known(*b), known(*c)])
}

/// `name` as git takes it where `core.precomposeUnicode` is set; `None`
/// where that is the name as it stands. A name the conversion fails on
/// stands: one that is not UTF-8, and one with a code point above
/// U+FFFD, which that decoder does not take.
pub fn composed(name: &[u8]) -> Option<Vec<u8>> {
    if name.is_ascii() {
        return None;
    }
    let text = std::str::from_utf8(name).ok()?;
    let mut out = String::with_capacity(name.len());
    let mut last: Option<char> = None;
    for next in text.chars() {
        if u32::from(next) > 0xFFFD {
            return None;
        }
        let joined = last.and_then(|last| pair(u32::from(last), u32::from(next)));
        match joined.and_then(char::from_u32) {
            Some(joined) => last = Some(joined),
            None => {
                out.extend(last);
                last = Some(next);
            }
        }
    }
    out.extend(last);
    (out.as_bytes() != name).then(|| out.into_bytes())
}
