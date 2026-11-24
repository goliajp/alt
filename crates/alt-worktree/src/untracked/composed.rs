//! `core.precomposeUnicode`: a file system that lists a name decomposed
//! reaches the file by the composed name as well, and git takes every
//! name it reads from a directory in its composed form there.

use crate::fsdir::Listing;

/// `name` in Normalization Form C, where it is text that is not in that
/// form already.
pub fn composed(name: &[u8]) -> Option<Vec<u8>> {
    if name.is_ascii() {
        return None;
    }
    let composed = alt_url::nfc(std::str::from_utf8(name).ok()?).into_bytes();
    (composed != name).then_some(composed)
}

impl Listing {
    /// [`add`](Self::add), with the name composed where `precompose`
    /// asks for it.
    pub(crate) fn take(
        &mut self,
        name: &[u8],
        d_type: u8,
        precompose: bool,
        unknown: &mut Vec<usize>,
    ) {
        let text = &name[..name.len() - 1];
        match composed(text).filter(|_| precompose) {
            Some(mut name) => {
                name.push(0);
                self.add(&name, d_type, unknown)
            }
            None => self.add(name, d_type, unknown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::composed;

    #[test]
    fn a_name_is_composed_where_it_is_text_in_another_form() {
        assert_eq!(composed(b"cafe\xcc\x81"), Some(b"caf\xc3\xa9".to_vec()));
        // a hangul syllable, and marks that are not in canonical order
        let syllable = "\u{1100}\u{1161}\u{11a8}".as_bytes();
        assert_eq!(composed(syllable), Some("\u{ac01}".as_bytes().to_vec()));
        let marks = "a\u{315}\u{300}".as_bytes();
        assert_eq!(composed(marks), Some("\u{e0}\u{315}".as_bytes().to_vec()));
        for as_it_is in [&b"cafe"[..], b"caf\xc3\xa9", b"caf\xe9", b""] {
            assert_eq!(composed(as_it_is), None);
        }
    }
}
