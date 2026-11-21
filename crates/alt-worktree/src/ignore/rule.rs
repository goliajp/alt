//! One line of an ignore file, and how a path is matched against it.
//!
//! A pattern is held against the bytes of a path as they are.
//! `core.ignorecase` is not read: where git has it set, as it sets it
//! itself on a file system that folds case, git matches a rule whatever
//! the case of its letters, and a rule here does not.

use crate::glob::glob_match;

/// One parsed `.gitignore` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    /// The pattern in its post-strip form (no leading `!`, no trailing `/`).
    pub(super) pattern: Vec<u8>,
    pub(super) negate: bool,
    pub(super) dir_only: bool,
    /// True if the pattern contains a `/` other than a trailing one — i.e.
    /// it should match against the full relative path, not just the basename.
    pub(super) anchored: bool,
    form: Form,
}

/// What it takes to match a pattern, told once when the rule is read:
/// most rules of an ignore file name a file or an ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// no wildcard: the text itself
    Literal,
    /// one `*` and then text without a wildcard: what ends in that text
    Ending,
    Glob,
}

fn wild(text: &[u8]) -> bool {
    text.iter().any(|b| matches!(b, b'*' | b'?' | b'[' | b'\\'))
}

/// Which part of a path a pattern without a `/` is held against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Against {
    /// the name of the path: the walk has asked about every directory
    /// above it already, as git does
    Name,
    /// each of its components
    Components,
}

impl Rule {
    /// The rule one line of an ignore file states, when it states one.
    pub(super) fn parse(line: &[u8]) -> Option<Rule> {
        let line = strip_trailing_spaces(strip_cr(line));
        if line.is_empty() || line[0] == b'#' {
            return None;
        }
        let (negate, body) = match line.strip_prefix(b"!") {
            Some(body) => (true, body),
            None => (false, line),
        };
        let (body, dir_only) = match body.strip_suffix(b"/") {
            Some(body) => (body, true),
            None => (body, false),
        };
        if body.is_empty() {
            return None;
        }
        // a pattern with a `/` inside it is anchored as well
        let (anchored, body) = match body.strip_prefix(b"/") {
            Some(body) => (true, body),
            None => (body.contains(&b'/'), body),
        };
        let form = match body {
            _ if !wild(body) => Form::Literal,
            [b'*', ending @ ..] if !anchored && !wild(ending) => Form::Ending,
            _ => Form::Glob,
        };
        Some(Rule {
            pattern: body.to_vec(),
            negate,
            dir_only,
            anchored,
            form,
        })
    }

    /// Whether the rule names one path and nothing else.
    pub(super) fn exact(&self) -> bool {
        self.anchored && self.form == Form::Literal
    }

    /// `rel` is the candidate path relative to the rule's layer base.
    pub(super) fn matches(&self, rel: &[u8], against: Against) -> bool {
        if self.anchored {
            return match self.form {
                Form::Literal => rel == self.pattern,
                _ => glob_match(&self.pattern, rel),
            };
        }
        match against {
            Against::Name => {
                let name = rel.iter().rposition(|&b| b == b'/').map_or(0, |at| at + 1);
                self.matches_name(&rel[name..])
            }
            // the same as `**/pattern` with anything beneath
            Against::Components => rel.split(|&b| b == b'/').any(|seg| self.matches_name(seg)),
        }
    }

    fn matches_name(&self, name: &[u8]) -> bool {
        match self.form {
            Form::Literal => name == self.pattern,
            Form::Ending => name.ends_with(&self.pattern[1..]),
            Form::Glob => glob_match(&self.pattern, name),
        }
    }
}

fn strip_cr(line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    }
}

/// `line` without the spaces it ends in, as git takes them off: a space
/// after a backslash stays, with what is before it, and so does a tab.
fn strip_trailing_spaces(line: &[u8]) -> &[u8] {
    let (mut at, mut spaces) = (0, None);
    while at < line.len() {
        match line[at] {
            b' ' => spaces = spaces.or(Some(at)),
            // what follows it is taken as it is; nothing follows the last
            b'\\' if at + 1 == line.len() => return line,
            b'\\' => (at, spaces) = (at + 1, None),
            _ => spaces = None,
        }
        at += 1;
    }
    &line[..spaces.unwrap_or(line.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaces_at_the_end_of_a_line_go_unless_a_backslash_keeps_one_and_a_tab_stays() {
        let kept = |line: &str| {
            let kept = strip_trailing_spaces(line.as_bytes());
            String::from_utf8(kept.to_vec()).unwrap()
        };
        for (line, left) in [
            ("a.txt  ", "a.txt"),
            ("a b  ", "a b"),
            ("a\\ ", "a\\ "),
            ("a\\   ", "a\\ "),
            ("a\t", "a\t"),
            ("a \t", "a \t"),
            ("a\t ", "a\t"),
            ("a \\", "a \\"),
            ("   ", ""),
        ] {
            assert_eq!(kept(line), left, "{line:?}");
        }
        let rule = Rule::parse(b"kept\\ ").unwrap();
        assert!(rule.matches(b"kept ", Against::Name));
        assert!(!rule.matches(b"kept", Against::Name));
        let rule = Rule::parse(b"tab.txt\t").unwrap();
        assert!(rule.matches(b"tab.txt\t", Against::Name));
        assert!(!rule.matches(b"tab.txt", Against::Name));
    }

    #[test]
    fn a_rule_matches_as_its_pattern_does_whatever_form_it_was_told_to_have() {
        let patterns = [
            "rustc",
            "*.swp",
            "*",
            "*.",
            "a*b",
            "*.[ch]",
            "?x",
            "/top",
            "/src/*.pyc",
            "a/b",
            "**/deep",
            "a\\*",
            "*\\.x",
            ".idea",
        ];
        let paths = [
            "rustc",
            "x.swp",
            ".swp",
            "swp",
            "a/x.swp",
            "a.swp/b",
            "ab",
            "axb",
            "a/b",
            "top",
            "a/top",
            "src/x.pyc",
            "src/a/x.pyc",
            "deep",
            "a/deep",
            "a*",
            "x.c",
            ".idea",
            "a.",
            "",
            "x/rustc",
            "rustc/x",
            "a/b/c",
        ];
        for pattern in patterns {
            let rule = Rule::parse(pattern.as_bytes()).unwrap();
            let glob = |text: &[u8]| glob_match(&rule.pattern, text);
            for path in paths {
                let path = path.as_bytes();
                let name = path.rsplit(|&b| b == b'/').next().unwrap();
                let (by_name, by_parts) = match rule.anchored {
                    true => (glob(path), glob(path)),
                    false => (glob(name), path.split(|&b| b == b'/').any(glob)),
                };
                let said = |against| rule.matches(path, against);
                assert_eq!(said(Against::Name), by_name, "{pattern} {path:?}");
                assert_eq!(said(Against::Components), by_parts, "{pattern} {path:?}");
            }
        }
        let form = |pattern: &str| Rule::parse(pattern.as_bytes()).unwrap().form;
        assert_eq!(form("rustc"), Form::Literal);
        assert_eq!(form("/src/doc/build"), Form::Literal);
        assert_eq!(form("*.swp"), Form::Ending);
        assert_eq!(form("/src/etc/*.pyc"), Form::Glob);
        assert_eq!(form("/*.pyc"), Form::Glob);
        assert_eq!(form("*.[ch]"), Form::Glob);
    }
}
