//! Texts of characters: every character against every class, and case.

use alt_regex::{Options, Regex};

use super::{Theirs, in_utf8_locale};

fn encoded(c: char) -> Vec<u8> {
    c.to_string().into_bytes()
}

/// Both verdicts on whether `pattern` matches `text`, when they differ.
fn differs(ours: &Regex, theirs: &Theirs, text: &[u8]) -> bool {
    ours.is_match(text) != theirs.spans(text).is_some()
}

const CLASSES: [&[u8]; 20] = [
    b"^[[:alpha:]]$",
    b"^[[:digit:]]$",
    b"^[[:alnum:]]$",
    b"^[[:upper:]]$",
    b"^[[:lower:]]$",
    b"^[[:space:]]$",
    b"^[[:blank:]]$",
    b"^[[:punct:]]$",
    b"^[[:print:]]$",
    b"^[[:graph:]]$",
    b"^[[:cntrl:]]$",
    b"^[[:xdigit:]]$",
    b"^\\w$",
    b"^\\W$",
    b"^\\s$",
    b"^.$",
    b"^[^[:alpha:]]$",
    b"^[\x80-\xff\xe2\x82\xac]$",
    b"^x\\b.$",
    b"^x\\B.$",
];

/// Compares every `step`th character from `from` on against each class.
fn sweep(from: u32, step: usize, ignore_case: bool) -> Vec<String> {
    in_utf8_locale();
    let options = Options {
        utf8: true,
        ignore_case,
        ..Options::default()
    };
    let mut differ = Vec::new();
    for pattern in CLASSES {
        let ours = Regex::with_options(pattern, options).unwrap();
        let theirs = Theirs::new(pattern, options);
        for c in (from..=0x10ffff).step_by(step).filter_map(char::from_u32) {
            let mut text = encoded(c);
            if pattern.starts_with(b"^x") {
                text.insert(0, b'x');
            }
            if differs(&ours, &theirs, &text) && differ.len() < 40 {
                differ.push(format!("{} on U+{:04X}", pattern.escape_ascii(), c as u32));
            }
        }
    }
    differ
}

#[test]
fn the_classes_hold_the_characters_the_c_library_puts_in_them() {
    for ignore_case in [false, true] {
        let differ = sweep(1, 23, ignore_case);
        assert!(differ.is_empty(), "{}", differ.join("\n"));
    }
}

#[test]
#[ignore = "every character against every class in both cases, minutes without --release"]
fn the_classes_hold_the_characters_the_c_library_puts_in_them_long() {
    for ignore_case in [false, true] {
        let differ = sweep(1, 1, ignore_case);
        assert!(differ.is_empty(), "{}", differ.join("\n"));
    }
}

#[test]
fn case_is_ignored_for_the_characters_the_c_library_ignores_it_for() {
    in_utf8_locale();
    let options = Options {
        utf8: true,
        ignore_case: true,
        ..Options::default()
    };
    let mut differ = Vec::new();
    let mut compared = 0;
    for c in (1..=0x1ffff).filter_map(char::from_u32) {
        let others: Vec<char> = c.to_uppercase().chain(c.to_lowercase()).collect();
        if others.iter().all(|&other| other == c) && !c.is_ascii() {
            continue;
        }
        let quoted = match c.is_ascii_punctuation() {
            true => format!("^\\{c}$"),
            false => format!("^{c}$"),
        };
        let listed = format!("^[{c}x]$");
        let listed = (c != ']' && c != '^').then_some(listed);
        for pattern in [Some(quoted), listed].into_iter().flatten() {
            let ours = Regex::with_options(pattern.as_bytes(), options);
            let theirs = Theirs::new(pattern.as_bytes(), options);
            let Ok(ours) = ours else {
                assert!(theirs.0.is_none(), "{pattern}");
                continue;
            };
            assert!(theirs.0.is_some(), "{pattern}");
            for &text in others
                .iter()
                .chain(&[c, 'I', 'ı', 'S', 'ſ', 'K', '\u{212a}'])
            {
                compared += 1;
                if differs(&ours, &theirs, &encoded(text)) && differ.len() < 40 {
                    differ.push(format!("{pattern} on U+{:04X}", text as u32));
                }
            }
        }
    }
    assert!(compared > 20_000, "{compared}");
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}

#[test]
fn bytes_that_are_no_character_match_as_in_the_c_library() {
    in_utf8_locale();
    let patterns: [&[u8]; 16] = [
        b".",
        b"^.$",
        b"^..$",
        b"[^a]",
        b"^[^a]*$",
        b"\\W",
        b"caf\xe9",
        b"caf[\xe9]",
        b"^caf.\\(",
        b"^[a-z]+\\>",
        b"\\<caf",
        b"f\\b",
        b"f\\B",
        b"^[[:alpha:]_]+[^[:alpha:]]",
        b"(.)\\1",
        "^é".as_bytes(),
    ];
    let texts: [&[u8]; 12] = [
        b"caf\xe9(void)",
        "café(void)".as_bytes(),
        b"\xe9",
        b"\xc3",
        b"\xa9\xa9",
        b"\xed\xa0\x80",
        b"\xf4\x90\x80\x80",
        b"\xf8\x88\x80\x80\x80",
        b"\xc0\x80",
        b"caf\xa9 x",
        b"f\xaa",
        "éé".as_bytes(),
    ];
    let mut differ = Vec::new();
    for ignore_case in [false, true] {
        let options = Options {
            utf8: true,
            ignore_case,
            ..Options::default()
        };
        for pattern in patterns {
            let ours = Regex::with_options(pattern, options).unwrap();
            let theirs = Theirs::new(pattern, options);
            for text in texts {
                let found = ours.find(text).map(|r| (r.start, r.end));
                let expected = theirs.spans(text).and_then(|spans| spans[0]);
                if found != expected {
                    let (pattern, text) = (pattern.escape_ascii(), text.escape_ascii());
                    differ.push(format!("{pattern} on {text}: {found:?} not {expected:?}"));
                }
            }
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}
