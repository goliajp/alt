//! The wildcards of a pathspec, as git matches them.

/// How a pattern is read.
#[derive(Clone, Copy)]
pub(super) struct Mode {
    /// `*` and `?` stop at a `/`, and `**` between slashes stands for any
    /// number of directories
    pub(super) pathname: bool,
    pub(super) fold_case: bool,
}

impl Mode {
    fn same(self, a: u8, b: u8) -> bool {
        match self.fold_case {
            true => a.eq_ignore_ascii_case(&b),
            false => a == b,
        }
    }

    /// Whether all of `text` is what `pattern` describes; `at_start` when
    /// both begin where a name of the path begins.
    pub(super) fn matches(self, pattern: &[u8], text: &[u8], at_start: bool) -> bool {
        let Some((&first, rest)) = pattern.split_first() else {
            return text.is_empty();
        };
        match first {
            b'*' => self.star(rest, text, at_start),
            b'?' => match text.split_first() {
                Some((&c, after)) => {
                    !(self.pathname && c == b'/') && self.matches(rest, after, false)
                }
                None => false,
            },
            b'[' => match (class(rest), text.split_first()) {
                (Some((set, rest)), Some((&c, after))) => {
                    !(self.pathname && c == b'/')
                        && set.has(c, self)
                        && self.matches(rest, after, false)
                }
                _ => false,
            },
            _ => {
                // a backslash takes what follows it as it is
                let (literal, rest) = match (first, rest.split_first()) {
                    (b'\\', Some((&next, rest))) => (next, rest),
                    _ => (first, rest),
                };
                match text.split_first() {
                    Some((&c, after)) => {
                        self.same(literal, c) && self.matches(rest, after, c == b'/')
                    }
                    None => false,
                }
            }
        }
    }

    /// What follows a `*` in the pattern, against `text`.
    fn star(self, rest: &[u8], text: &[u8], at_start: bool) -> bool {
        let more = rest.iter().take_while(|b| **b == b'*').count();
        let after = &rest[more..];
        let across = match self.pathname {
            false => true,
            true => more > 0 && at_start && matches!(after.first(), None | Some(b'/')),
        };
        if across && self.pathname {
            // `**/` is no directory or several
            if let Some(names) = after.strip_prefix(b"/")
                && self.matches(names, text, true)
            {
                return true;
            }
            if after.is_empty() {
                return true;
            }
        }
        (0..=text.len())
            .take_while(|&at| across || !text[..at].contains(&b'/'))
            .any(|at| {
                let begins = at > 0 && text[at - 1] == b'/' || at == 0 && at_start;
                self.matches(after, &text[at..], begins)
            })
    }
}

/// A set of characters, as written between `[` and `]`.
struct Set<'a> {
    negated: bool,
    body: &'a [u8],
}

/// The set a pattern begins with after its `[`, and what follows the `]`.
fn class(pattern: &[u8]) -> Option<(Set<'_>, &[u8])> {
    let negated = matches!(pattern.first(), Some(b'!' | b'^'));
    let body = &pattern[negated as usize..];
    let mut at = 0;
    loop {
        match *body.get(at)? {
            b']' if at > 0 => break,
            b'\\' => at += 2,
            _ => at += 1,
        }
    }
    let set = Set {
        negated,
        body: &body[..at],
    };
    Some((set, &body[at + 1..]))
}

impl Set<'_> {
    fn has(&self, c: u8, mode: Mode) -> bool {
        let mut members = Vec::new();
        let mut body = self.body.iter().copied().peekable();
        while let Some(b) = body.next() {
            match b {
                b'\\' => members.push(body.next().unwrap_or(b'\\')),
                b'-' if !members.is_empty() && body.peek().is_some() => {
                    let from: u8 = *members.last().unwrap();
                    let to = match body.next() {
                        Some(b'\\') => body.next().unwrap_or(b'\\'),
                        to => to.unwrap_or(from),
                    };
                    members.extend(from..=to);
                }
                b => members.push(b),
            }
        }
        members.iter().any(|m| mode.same(*m, c)) != self.negated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pathname: bool, pattern: &str, text: &str) -> bool {
        let mode = Mode {
            pathname,
            fold_case: false,
        };
        mode.matches(pattern.as_bytes(), text.as_bytes(), true)
    }

    #[test]
    fn a_star_crosses_a_slash_only_where_names_are_not_told_apart() {
        assert!(m(false, "*.txt", "a/b.txt"));
        assert!(!m(true, "*.txt", "a/b.txt"));
        assert!(m(true, "*.txt", "b.txt"));
        assert!(m(true, "**/*.txt", "a/b/c.txt"));
        assert!(m(true, "**/*.txt", "c.txt"));
        assert!(m(true, "a/**", "a/b/c"));
        assert!(m(true, "a/**/c", "a/c"));
        assert!(m(true, "a/**/c", "a/x/y/c"));
        assert!(!m(true, "a**/c", "a/x/c"));
        assert!(m(true, "a**c", "abc"));
        assert!(m(false, "de*", "deep/g.txt"));
        assert!(!m(true, "de*", "deep/g.txt"));
    }

    #[test]
    fn sets_marks_and_escapes_are_read_as_git_reads_them() {
        assert!(m(false, "f?.txt", "f2.txt"));
        assert!(!m(true, "f?g", "f/g"));
        assert!(m(false, "[fg]2.txt", "g2.txt"));
        assert!(!m(false, "[!fg]2.txt", "g2.txt"));
        assert!(m(false, "[a-c]x", "bx"));
        assert!(m(false, "[]]x", "]x"));
        assert!(m(false, "[\\]a]x", "]x"));
        assert!(m(false, "\\*.txt", "*.txt"));
        assert!(!m(false, "\\*.txt", "a.txt"));
        assert!(!m(false, "[ab", "a"));
        let fold = Mode {
            pathname: false,
            fold_case: true,
        };
        assert!(fold.matches(b"F[A-C].TXT", b"fb.txt", true));
    }
}
