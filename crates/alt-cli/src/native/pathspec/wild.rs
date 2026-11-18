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
            b'[' => match text.split_first() {
                Some((&c, after)) if !(self.pathname && c == b'/') => match self.in_set(pattern, c)
                {
                    Some((true, end)) => self.matches(&pattern[end..], after, false),
                    _ => false,
                },
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

    /// Whether `c` is in the set `pattern` begins with, and where the
    /// pattern goes on after it; `None` for a set that is not closed. With
    /// case folded, a letter is in a set that has it in either case.
    fn in_set(self, pattern: &[u8], c: u8) -> Option<(bool, usize)> {
        let (is, end) = alt_worktree::class_at(pattern, 0, c)?;
        if !self.fold_case {
            return Some((is, end));
        }
        let other = match c.is_ascii_lowercase() {
            true => c.to_ascii_uppercase(),
            false => c.to_ascii_lowercase(),
        };
        let (other, _) = alt_worktree::class_at(pattern, 0, other)?;
        // a set that leaves out has `c` when it leaves out neither case
        let leaves_out = matches!(pattern.get(1), Some(b'!' | b'^'));
        Some((if leaves_out { is && other } else { is || other }, end))
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
        // what follows a range is not the start of another
        assert!(m(false, "[a-c-e]x", "-x") && m(false, "[a-c-e]x", "ex"));
        assert!(!m(false, "[a-c-e]x", "dx"));
        assert!(m(false, "[[:alpha:]][[:digit:]]", "a1") && !m(false, "[[:alpha:]]", "1"));
        assert!(m(false, "[![:digit:]]", "a") && !m(false, "[^[:digit:]x]", "x"));
        let fold = Mode {
            pathname: false,
            fold_case: true,
        };
        assert!(fold.matches(b"F[A-C].TXT", b"fb.txt", true));
        assert!(!fold.matches(b"[!a-c]", b"B", true));
    }
}
