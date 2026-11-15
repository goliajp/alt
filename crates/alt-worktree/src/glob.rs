//! Matching one glob pattern against a path, as `.gitignore` and
//! `.gitattributes` patterns are matched.

/// Glob-match `text` against `pattern`. Supports `*` (any run not crossing
/// `/`), `?` (one char not `/`), `[...]` classes, `\x` for a literal `x`,
/// and `**` (any number of path components, including across `/`).
pub(crate) fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    // Handle leading "**/" as a special prefix: it accepts a path with
    // any number of leading components.
    if pattern.starts_with(b"**/") {
        // `**/foo` matches both `foo` and `bar/foo` (any depth).
        let rest = &pattern[3..];
        if glob_match(rest, text) {
            return true;
        }
        // try every position right after a `/` boundary
        for (i, &b) in text.iter().enumerate() {
            if b == b'/' && glob_match(pattern, &text[i + 1..]) {
                return true;
            }
        }
        return false;
    }
    glob_match_segment(pattern, text)
}

/// Standard non-`**` segment matcher. `*` is bounded by `/`.
fn glob_match_segment(pat: &[u8], text: &[u8]) -> bool {
    fn rec(pat: &[u8], text: &[u8]) -> bool {
        let mut pi = 0;
        let mut ti = 0;
        let mut star_pi: Option<usize> = None;
        let mut star_ti = 0;
        while ti < text.len() {
            if pi < pat.len() {
                let pc = pat[pi];
                let tc = text[ti];
                if pc == b'*' {
                    // Look ahead: a `**` segment in the middle of the
                    // pattern means "match any path tail of components".
                    if pi + 1 < pat.len() && pat[pi + 1] == b'*' {
                        let after = pi + 2;
                        // Zero-component case: `**` consumes nothing,
                        // optionally swallowing the `/` that follows so
                        // `a/**/b` still matches `a/b`.
                        let zero_pat = if after < pat.len() && pat[after] == b'/' {
                            &pat[after + 1..]
                        } else {
                            &pat[after..]
                        };
                        if rec(zero_pat, &text[ti..]) {
                            return true;
                        }
                        // One-or-more components case
                        for k in ti..=text.len() {
                            if rec(&pat[after..], &text[k..]) {
                                return true;
                            }
                        }
                        return false;
                    }
                    star_pi = Some(pi);
                    star_ti = ti;
                    pi += 1;
                    continue;
                }
                // where the pattern continues once `tc` is consumed
                let next = match pc {
                    // neither `?` nor a class crosses `/`
                    b'?' => (tc != b'/').then_some(pi + 1),
                    b'[' => match class_at(pat, pi, tc) {
                        Some((hit, after)) => (hit && tc != b'/').then_some(after),
                        None => (tc == b'[').then_some(pi + 1),
                    },
                    b'\\' if pi + 1 < pat.len() => (pat[pi + 1] == tc).then_some(pi + 2),
                    _ => (pc == tc).then_some(pi + 1),
                };
                if let Some(next) = next {
                    pi = next;
                    ti += 1;
                    continue;
                }
            }
            // backtrack to last `*`
            if let Some(sp) = star_pi {
                if text[star_ti] == b'/' {
                    // `*` can't consume `/`; fail
                    return false;
                }
                pi = sp + 1;
                star_ti += 1;
                ti = star_ti;
                continue;
            }
            return false;
        }
        // consume trailing `*`s
        while pi < pat.len() && pat[pi] == b'*' {
            pi += 1;
        }
        pi == pat.len()
    }
    rec(pat, text)
}

/// Matches `c` against the bracket class that opens at `pat[at]`: whether
/// it is in the class, and where the pattern continues after the closing
/// `]`. `None` when the class is never closed, which makes `[` literal.
fn class_at(pat: &[u8], at: usize, c: u8) -> Option<(bool, usize)> {
    let mut i = at + 1;
    let negate = matches!(pat.get(i), Some(b'!' | b'^'));
    if negate {
        i += 1;
    }
    let first = i;
    let mut hit = false;
    loop {
        let lo = *pat.get(i)?;
        if lo == b']' && i > first {
            return Some((hit != negate, i + 1));
        }
        if lo == b'[' && pat.get(i + 1) == Some(&b':') {
            let end = i + 2 + pat[i + 2..].windows(2).position(|w| w == b":]")?;
            hit |= named_class(&pat[i + 2..end], c);
            i = end + 2;
            continue;
        }
        let lo = if lo == b'\\' {
            i += 1;
            *pat.get(i)?
        } else {
            lo
        };
        match (pat.get(i + 1), pat.get(i + 2)) {
            (Some(b'-'), Some(&hi)) if hi != b']' => {
                hit |= (lo..=hi).contains(&c);
                i += 3;
            }
            _ => {
                hit |= lo == c;
                i += 1;
            }
        }
    }
}

fn named_class(name: &[u8], c: u8) -> bool {
    match name {
        b"alpha" => c.is_ascii_alphabetic(),
        b"digit" => c.is_ascii_digit(),
        b"alnum" => c.is_ascii_alphanumeric(),
        b"upper" => c.is_ascii_uppercase(),
        b"lower" => c.is_ascii_lowercase(),
        b"space" => c.is_ascii_whitespace() || c == 0x0b,
        b"blank" => c == b' ' || c == b'\t',
        b"punct" => c.is_ascii_punctuation(),
        b"xdigit" => c.is_ascii_hexdigit(),
        b"cntrl" => c.is_ascii_control(),
        b"print" => (0x20..0x7f).contains(&c),
        b"graph" => (0x21..0x7f).contains(&c),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracket_classes_match_one_character() {
        assert!(glob_match(b"*.[ch]", b"blame.c"));
        assert!(glob_match(b"*.[ch]", b"blame.h"));
        assert!(!glob_match(b"*.[ch]", b"blame.o"));
        assert!(!glob_match(b"*.[ch]", b"blame.ch"));
        assert!(glob_match(b"t[0-9][0-9]-*.sh", b"t12-x.sh"));
        assert!(!glob_match(b"t[0-9][0-9]-*.sh", b"t1a-x.sh"));
        assert!(glob_match(b"[!a-c]x", b"dx"));
        assert!(!glob_match(b"[!a-c]x", b"bx"));
        assert!(glob_match(b"[]x]y", b"]y"));
        assert!(glob_match(b"[[:digit:]]*", b"7up"));
        assert!(
            !glob_match(b"a[b/]c", b"a/c"),
            "a class never matches a slash"
        );
        assert!(glob_match(b"a[bc", b"a[bc"), "an unclosed class is literal");
        assert!(glob_match(b"a\\*b", b"a*b"));
        assert!(!glob_match(b"a\\*b", b"axb"));
    }
}
