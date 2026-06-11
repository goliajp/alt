//! Pathname-aware glob matching for `includeIf` conditions, following
//! git's wildmatch semantics: `*` and `?` do not cross `/`, `**` does but
//! only when slash-bounded (`a/**/b`, `**/b`, `a/**`) — an unbounded `**`
//! degrades to `*`. Character classes support ranges and `!`/`^` negation.
//! (POSIX named classes like `[[:alpha:]]` are not supported yet.)

pub fn wildmatch(pattern: &[u8], text: &[u8], case_insensitive: bool) -> bool {
    Matcher { case_insensitive }.matches(pattern, text, true)
}

struct Matcher {
    case_insensitive: bool,
}

impl Matcher {
    fn eq(&self, a: u8, b: u8) -> bool {
        if self.case_insensitive {
            a.eq_ignore_ascii_case(&b)
        } else {
            a == b
        }
    }

    fn matches(&self, pat: &[u8], text: &[u8], at_path_start: bool) -> bool {
        match pat.first() {
            None => text.is_empty(),
            Some(b'*') => {
                let double = pat.get(1) == Some(&b'*');
                let bounded_left = at_path_start;
                let rest_after = if double { &pat[2..] } else { &pat[1..] };
                let bounded_right = rest_after.is_empty() || rest_after.first() == Some(&b'/');
                if double && bounded_left && bounded_right {
                    // slash-bounded `**`: any number of whole segments
                    if rest_after.is_empty() {
                        return true; // trailing `/**` swallows the rest
                    }
                    // zero segments: `**/x` may match plain `x`
                    if self.matches(&rest_after[1..], text, true) {
                        return true;
                    }
                    // one or more segments: align the `/rest` at any slash
                    for i in 0..text.len() {
                        if text[i] == b'/' && self.matches(rest_after, &text[i..], false) {
                            return true;
                        }
                    }
                    false
                } else {
                    // `*` (or unbounded `**` degraded): within one segment
                    let rest = if double { &pat[2..] } else { &pat[1..] };
                    for i in 0..=text.len() {
                        if self.matches(rest, &text[i..], at_path_start && i == 0) {
                            return true;
                        }
                        if i < text.len() && text[i] == b'/' {
                            break;
                        }
                    }
                    false
                }
            }
            Some(b'?') => {
                !text.is_empty() && text[0] != b'/' && self.matches(&pat[1..], &text[1..], false)
            }
            Some(b'[') => {
                let Some((matched, rest)) = self.match_class(&pat[1..], text) else {
                    return false;
                };
                matched && self.matches(rest, &text[1..], false)
            }
            Some(&c) => {
                !text.is_empty()
                    && self.eq(c, text[0])
                    && self.matches(&pat[1..], &text[1..], c == b'/')
            }
        }
    }

    /// Matches `text[0]` against the class body; returns (hit, rest-of-pattern).
    fn match_class<'p>(&self, class: &'p [u8], text: &[u8]) -> Option<(bool, &'p [u8])> {
        let &ch = text.first()?;
        if ch == b'/' {
            return None; // classes never match a slash
        }
        let mut i = 0;
        let negated = matches!(class.first(), Some(b'!') | Some(b'^'));
        if negated {
            i += 1;
        }
        let mut hit = false;
        let mut first = true;
        loop {
            let &c = class.get(i)?;
            if c == b']' && !first {
                return Some((hit != negated, &class[i + 1..]));
            }
            first = false;
            if class.get(i + 1) == Some(&b'-') && class.get(i + 2).is_some_and(|&e| e != b']') {
                let lo = c;
                let hi = class[i + 2];
                let probe = if self.case_insensitive {
                    ch.to_ascii_lowercase()
                } else {
                    ch
                };
                let (lo, hi) = if self.case_insensitive {
                    (lo.to_ascii_lowercase(), hi.to_ascii_lowercase())
                } else {
                    (lo, hi)
                };
                if (lo..=hi).contains(&probe) {
                    hit = true;
                }
                i += 3;
            } else {
                if self.eq(c, ch) {
                    hit = true;
                }
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, t: &str) -> bool {
        wildmatch(p.as_bytes(), t.as_bytes(), false)
    }

    #[test]
    fn single_star_stays_in_segment() {
        assert!(m("a/*.rs", "a/x.rs"));
        assert!(!m("a/*.rs", "a/b/x.rs"));
        assert!(m("*", "abc"));
        assert!(!m("*", "a/b"));
    }

    #[test]
    fn double_star_crosses_segments() {
        assert!(m("**/x", "x"));
        assert!(m("**/x", "a/b/x"));
        assert!(m("a/**", "a/b/c"));
        assert!(m("a/**/z", "a/z"));
        assert!(m("a/**/z", "a/b/c/z"));
        assert!(!m("a/**/z", "a/b/c/y"));
        // unbounded ** degrades to *
        assert!(m("a**b", "axxb"));
        assert!(!m("a**b", "ax/xb"));
    }

    #[test]
    fn question_and_classes() {
        assert!(m("a?c", "abc"));
        assert!(!m("a?c", "a/c"));
        assert!(m("[a-c]x", "bx"));
        assert!(!m("[!a-c]x", "bx"));
        assert!(m("[!a-c]x", "dx"));
        assert!(m("[]]x", "]x"));
        assert!(!m("[abc", "a")); // unterminated class never matches
    }

    #[test]
    fn case_folding() {
        assert!(!m("AbC", "abc"));
        assert!(wildmatch(b"AbC", b"abc", true));
        assert!(wildmatch(b"[A-Z]", b"q", true));
    }
}
