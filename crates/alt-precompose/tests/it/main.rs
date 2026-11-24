//! Names as the `UTF-8-MAC` decoder of macOS gives them, which git
//! records: each case here was read off that decoder.

use alt_precompose::composed;

fn becomes(name: &str, wanted: &str) {
    let got = composed(name.as_bytes()).map(|bytes| String::from_utf8(bytes).unwrap());
    assert_eq!(got.as_deref(), Some(wanted), "{name:?}");
}

fn stands(name: &[u8]) {
    assert_eq!(composed(name), None, "{name:?}");
}

#[test]
fn a_code_point_and_the_one_after_it_are_put_together() {
    becomes("cafe\u{301}", "caf\u{e9}");
    becomes("A\u{30a}", "\u{c5}");
    becomes("\u{304b}\u{3099}", "\u{304c}");
    // one after another, each with what the last became
    becomes("a\u{302}\u{301}", "\u{1ea5}");
    becomes("\u{e2}\u{301}", "\u{1ea5}");
    becomes("a\u{323}\u{302}", "\u{1ead}");
    // a syllable of its consonants and its vowel
    becomes("\u{1100}\u{1161}", "\u{ac00}");
    becomes("\u{1100}\u{1161}\u{11a8}", "\u{ac01}");
    becomes("\u{ac00}\u{11a8}", "\u{ac01}");
    becomes("\u{1112}\u{1175}\u{11c2}", "\u{d7a3}");
}

#[test]
fn nothing_is_reordered_and_nothing_is_replaced() {
    // marks that are not in canonical order: Form C would reorder them
    stands("a\u{315}\u{300}".as_bytes());
    stands("q\u{307}\u{323}".as_bytes());
    becomes("a\u{301}\u{302}", "\u{e1}\u{302}");
    // code points Form C replaces by another
    for alone in ["\u{212b}", "\u{f9dc}", "\u{37e}", "\u{2126}", "\u{958}"] {
        stands(alone.as_bytes());
    }
    // pairs Form C leaves apart, and pairs it joins that HFS+ does not
    // take apart
    stands("\u{915}\u{93c}".as_bytes());
    stands("\u{5d9}\u{5b4}".as_bytes());
    stands("\u{1b05}\u{1b35}".as_bytes());
    stands("\u{308}\u{301}".as_bytes());
    // a mark with nothing before it, and a syllable that has its end
    stands("\u{301}".as_bytes());
    stands("\u{ac01}\u{11a8}".as_bytes());
    stands("\u{1100}\u{11a8}".as_bytes());
}

#[test]
fn a_name_the_decoder_does_not_take_stands() {
    // above the sixteen bits it knows, with a pair it would join
    stands("\u{11099}\u{110ba}".as_bytes());
    stands("e\u{301}\u{1f600}".as_bytes());
    stands("e\u{301}\u{fffe}".as_bytes());
    stands(b"caf\xe9");
    stands(b"e\xcc\x81\xff");
    for as_it_is in [&b"cafe"[..], "caf\u{e9}".as_bytes(), b""] {
        stands(as_it_is);
    }
}
