//! Domain to ASCII for hosts that are not plain ASCII: UTS #46 mapping
//! without transitional processing, NFC, Punycode and label validation
//! with CheckBidi and CheckJoiners on, CheckHyphens and VerifyDnsLength off.
use crate::ParseError;

mod nfc;
mod punycode;
mod tables;
mod validate;

pub use tables::UNICODE_VERSION;

const PREFIX: [char; 4] = ['x', 'n', '-', '-'];

fn is_ascii(chars: &[char]) -> bool {
    chars.iter().all(char::is_ascii)
}

/// The index of the run that holds `code` in a table of sorted run starts.
fn run(table: &[u32], shift: u32, code: u32) -> usize {
    table.partition_point(|&entry| entry >> shift <= code) - 1
}

enum Mapping {
    Valid,
    Ignored,
    Disallowed,
    Text(&'static str),
    Char(char),
}

fn mapping(c: char) -> Mapping {
    let code = c as u32;
    let index = run(&tables::MAP_STARTS, 3, code);
    let (start, data) = (tables::MAP_STARTS[index], tables::MAP_DATA[index]);
    let mapped = |code: u32| Mapping::Char(char::from_u32(code).expect("mapping table"));
    match start & 7 {
        0 => Mapping::Valid,
        1 => Mapping::Ignored,
        2 => Mapping::Disallowed,
        3 => {
            let at = (data >> 6) as usize;
            Mapping::Text(&tables::MAP_TEXT[at..at + (data & 63) as usize])
        }
        4 => mapped(code.wrapping_add(data)),
        _ if (code - (start >> 3)).is_multiple_of(2) => mapped(code + 1),
        _ => Mapping::Valid,
    }
}

#[derive(Clone, Copy)]
struct Props(u32);

impl Props {
    fn of(c: char) -> Self {
        Self(tables::PROPS[run(&tables::PROPS, 11, c as u32)])
    }

    fn bidi(self) -> u32 {
        self.0 & 15
    }

    fn joining(self) -> u32 {
        self.0 >> 4 & 7
    }

    fn is_mark(self) -> bool {
        self.0 >> 7 & 1 != 0
    }

    fn nfc_quick_check_fails(self) -> bool {
        self.0 >> 8 & 1 != 0
    }

    fn combining_class(self, c: char) -> u8 {
        if self.0 >> 9 & 1 == 0 {
            return 0;
        }
        tables::CCC[run(&tables::CCC, 8, c as u32)] as u8
    }
}

fn combining_class(c: char) -> u8 {
    Props::of(c).combining_class(c)
}

fn map(domain: &str) -> Result<Vec<char>, ParseError> {
    let mut mapped = Vec::with_capacity(domain.len());
    for c in domain.chars() {
        if c.is_ascii() {
            mapped.push(c.to_ascii_lowercase());
            continue;
        }
        match mapping(c) {
            Mapping::Valid => mapped.push(c),
            Mapping::Ignored => {}
            Mapping::Disallowed => return Err(ParseError::IdnaError),
            Mapping::Text(text) => mapped.extend(text.chars()),
            Mapping::Char(c) => mapped.push(c),
        }
    }
    Ok(mapped)
}

/// Appends the ASCII form of `domain` to `out`.
pub(crate) fn to_ascii(domain: &str, out: &mut String) -> Result<(), ParseError> {
    let normalized = nfc::normalize(map(domain)?);
    let mut labels = Vec::with_capacity(normalized.len());
    for (index, label) in normalized.split(|&c| c == '.').enumerate() {
        if index > 0 {
            labels.push('.');
        }
        let Some(encoded) = label.strip_prefix(&PREFIX) else {
            labels.extend_from_slice(label);
            continue;
        };
        let start = labels.len();
        punycode::decode(encoded, &mut labels).ok_or(ParseError::IdnaError)?;
        validate::decoded(&labels[start..])?;
    }
    let bidi = labels.iter().any(|&c| validate::is_right_to_left(c));
    for (index, label) in labels.split(|&c| c == '.').enumerate() {
        if index > 0 {
            out.push('.');
        }
        validate::label(label, bidi)?;
        if is_ascii(label) {
            out.extend(label);
        } else {
            out.push_str("xn--");
            punycode::encode(label, out).ok_or(ParseError::IdnaError)?;
        }
    }
    Ok(())
}
