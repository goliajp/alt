//! Normalization Form C
use super::{Props, combining_class, tables};

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const S_COUNT: u32 = L_COUNT * V_COUNT * T_COUNT;

fn char_at(code: u32) -> char {
    char::from_u32(code).expect("normalization table")
}

/// Whether the quick check proves `chars` normalized; false means unknown.
fn quick_check(chars: &[char]) -> bool {
    let mut last = 0;
    for &c in chars {
        if (c as u32) < 0x300 {
            last = 0;
            continue;
        }
        let props = Props::of(c);
        let class = props.combining_class(c);
        if props.nfc_quick_check_fails() || (class != 0 && last > class) {
            return false;
        }
        last = class;
    }
    true
}

pub(super) fn normalize(chars: Vec<char>) -> Vec<char> {
    if quick_check(&chars) {
        chars
    } else {
        compose(&decomposed(&chars))
    }
}

pub(super) fn is_normalized(chars: &[char]) -> bool {
    quick_check(chars) || compose(&decomposed(chars)) == chars
}

fn decompose(code: u32, out: &mut Vec<char>) {
    if (S_BASE..S_BASE + S_COUNT).contains(&code) {
        let index = code - S_BASE;
        out.push(char_at(L_BASE + index / (V_COUNT * T_COUNT)));
        out.push(char_at(V_BASE + index % (V_COUNT * T_COUNT) / T_COUNT));
        if !index.is_multiple_of(T_COUNT) {
            out.push(char_at(T_BASE + index % T_COUNT));
        }
        return;
    }
    let found = tables::DECOMPOSITIONS.binary_search_by_key(&code, |entry| (entry >> 42) as u32);
    let Ok(index) = found else {
        out.push(char_at(code));
        return;
    };
    let entry = tables::DECOMPOSITIONS[index];
    decompose((entry >> 21) as u32 & 0x1F_FFFF, out);
    if entry & 0x1F_FFFF != 0 {
        decompose(entry as u32 & 0x1F_FFFF, out);
    }
}

/// The canonical decomposition of `chars` in canonical order.
fn decomposed(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len() + 8);
    for &c in chars {
        decompose(c as u32, &mut out);
    }
    let mut start = 0;
    while start < out.len() {
        let length = out[start..]
            .iter()
            .position(|&c| combining_class(c) == 0)
            .unwrap_or(out.len() - start);
        out[start..start + length].sort_by_key(|&c| combining_class(c));
        start += length + 1;
    }
    out
}

fn compose_pair(first: char, second: char) -> Option<char> {
    let (a, b) = (first as u32, second as u32);
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        return Some(char_at(
            S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT,
        ));
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&a)
        && (a - S_BASE).is_multiple_of(T_COUNT)
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&b)
    {
        return Some(char_at(a + b - T_BASE));
    }
    let key = u64::from(a) << 21 | u64::from(b);
    let index = tables::COMPOSITIONS
        .binary_search_by_key(&key, |entry| entry >> 21)
        .ok()?;
    Some(char_at(tables::COMPOSITIONS[index] as u32 & 0x1F_FFFF))
}

fn compose(chars: &[char]) -> Vec<char> {
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let mut starter = None;
    // the class of the last character kept after the starter, if any
    let mut last = None;
    for &c in chars {
        let class = combining_class(c);
        if let Some(starter) = starter
            && last.is_none_or(|last| last < class)
            && let Some(composite) = compose_pair(out[starter], c)
        {
            out[starter] = composite;
            continue;
        }
        if class == 0 {
            starter = Some(out.len());
            last = None;
        } else {
            last = Some(class);
        }
        out.push(c);
    }
    out
}
