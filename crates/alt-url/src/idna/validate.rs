use super::{Mapping, PREFIX, Props, combining_class, is_ascii, mapping, nfc};
use crate::ParseError;

const L: u32 = 1;
const R: u32 = 2;
const AL: u32 = 3;
const AN: u32 = 4;
const EN: u32 = 5;
const NSM: u32 = 11;

const JOINING_L: u32 = 1;
const JOINING_R: u32 = 2;
const JOINING_D: u32 = 3;
const JOINING_T: u32 = 4;

const VIRAMA: u8 = 9;
const ZWNJ: char = '\u{200C}';
const ZWJ: char = '\u{200D}';

pub(super) fn is_right_to_left(c: char) -> bool {
    !c.is_ascii() && matches!(Props::of(c).bidi(), R | AL | AN)
}

/// Checks a label decoded from Punycode, which mapping has not seen.
pub(super) fn decoded(label: &[char]) -> Result<(), ParseError> {
    let valid = |&c: &char| c.is_ascii() || matches!(mapping(c), Mapping::Valid);
    if is_ascii(label)
        || label.starts_with(&PREFIX)
        || !label.iter().all(valid)
        || !nfc::is_normalized(label)
    {
        return Err(ParseError::IdnaError);
    }
    Ok(())
}

pub(super) fn label(label: &[char], bidi_domain: bool) -> Result<(), ParseError> {
    let Some(&first) = label.first() else {
        return Ok(());
    };
    if Props::of(first).is_mark() || !joiners(label) || (bidi_domain && !bidi(label)) {
        return Err(ParseError::IdnaError);
    }
    Ok(())
}

/// The contextual rules of RFC 5892 for the two joiner characters.
fn joiners(label: &[char]) -> bool {
    label.iter().enumerate().all(|(index, &c)| {
        if c != ZWNJ && c != ZWJ {
            return true;
        }
        let (before, after) = (&label[..index], &label[index + 1..]);
        if before.last().is_some_and(|&c| combining_class(c) == VIRAMA) {
            return true;
        }
        let joining = |&c: &char| Props::of(c).joining();
        let left = before.iter().rev().map(joining).find(|&j| j != JOINING_T);
        let right = after.iter().map(joining).find(|&j| j != JOINING_T);
        c == ZWNJ
            && matches!(left, Some(JOINING_L | JOINING_D))
            && matches!(right, Some(JOINING_R | JOINING_D))
    })
}

/// The six rules of RFC 5893 section 2 for a label of a Bidi domain name.
fn bidi(label: &[char]) -> bool {
    let class = |&c: &char| Props::of(c).bidi();
    let last = label.iter().rev().map(class).find(|&class| class != NSM);
    match class(&label[0]) {
        L => {
            label.iter().map(class).all(|c| matches!(c, L | EN..=NSM))
                && matches!(last, Some(L | EN))
        }
        R | AL => {
            let has = |wanted| label.iter().map(class).any(|c| c == wanted);
            label.iter().map(class).all(|c| matches!(c, R..=NSM))
                && matches!(last, Some(R | AL | EN | AN))
                && !(has(EN) && has(AN))
        }
        _ => false,
    }
}
