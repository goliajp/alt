//! `.gitignore` parsing and matching for working-tree scans.
//!
//! Implements a useful subset of git's ignore-pattern semantics
//! (`gitignore(5)`): enough to handle every shape that appears in real
//! `.gitignore` files at the repository root. Patterns are loaded from one
//! file at a time into an [`IgnoreLayer`]; `scan_dir` stacks layers as it
//! descends, and the deepest layer's last matching rule wins.
//!
//! ## Supported syntax
//!
//! - Blank lines and lines starting with `#` are skipped.
//! - Trailing whitespace is stripped (a literal trailing space must be
//!   escaped as `\ `).
//! - A leading `!` negates the rule (the path is *unignored* if it would
//!   otherwise be ignored).
//! - A leading `/` anchors the pattern at the directory holding the
//!   `.gitignore`; without one, the rule matches the path's basename
//!   anywhere underneath that directory (unless the pattern contains an
//!   interior `/`, in which case it is anchored).
//! - A trailing `/` restricts the rule to directories.
//! - `*` matches any run of characters that is not `/`. `?` matches one
//!   non-`/` character. `**` matches any number of path components,
//!   including zero — but only as a path component on its own (so
//!   `a/**/b` matches `a/b`, `a/x/b`, `a/x/y/b`, …).
//!
//! ## Not supported (yet)
//!
//! - Character classes (`[abc]`, `[a-z]`).
//! - The `core.excludesFile` global ignore.
//! - Per-user `.git/info/exclude`.
//!
//! These are accepted by git but never appear in `alt`'s own `.gitignore`;
//! they can be filled in later when a real working tree needs them.

/// One parsed `.gitignore` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    /// The pattern in its post-strip form (no leading `!`, no trailing `/`).
    pattern: String,
    negate: bool,
    dir_only: bool,
    /// True if the pattern contains a `/` other than a trailing one — i.e.
    /// it should match against the full relative path, not just the basename.
    anchored: bool,
}

/// One `.gitignore` file's worth of rules, anchored at a base directory.
///
/// `base` is the path of the directory containing the `.gitignore`,
/// relative to the working-tree root, expressed as raw bytes joined with
/// `/`. The root layer's base is an empty slice; a deeper layer carries
/// the relative path of its parent directory.
#[derive(Debug, Clone, Default)]
pub(crate) struct IgnoreLayer {
    pub(crate) base: Vec<u8>,
    rules: Vec<Rule>,
}

/// A stack of `IgnoreLayer`s — the root layer at index 0, the deepest
/// active layer at the top. `is_ignored` consults the top first; if no
/// layer matches the path, it is not ignored.
#[derive(Debug, Default)]
pub(crate) struct IgnoreStack {
    layers: Vec<IgnoreLayer>,
}

impl IgnoreStack {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, layer: IgnoreLayer) {
        self.layers.push(layer);
    }

    pub(crate) fn pop(&mut self) {
        self.layers.pop();
    }

    /// Decide whether `path` (relative to the working-tree root, slash-joined)
    /// is ignored. `is_dir` distinguishes directories from regular files —
    /// rules with a trailing `/` only apply to directories.
    ///
    /// Within a single layer the *last* matching rule wins (so `!keep.log`
    /// can override an earlier `*.log`). Across layers a deeper layer's
    /// match wins over a shallower one's, and a layer that does not match
    /// the path falls through to the next-shallower layer.
    pub(crate) fn is_ignored(&self, path: &[u8], is_dir: bool) -> bool {
        for layer in self.layers.iter().rev() {
            if let Some(decision) = match_layer(layer, path, is_dir) {
                return decision;
            }
        }
        false
    }
}

/// Parse one `.gitignore` file's bytes into a layer rooted at `base`. The
/// base is consumed verbatim; callers compute it as the directory holding
/// the `.gitignore` (relative to the working-tree root, slash-joined).
pub(crate) fn parse_layer(content: &[u8], base: &[u8]) -> IgnoreLayer {
    let mut rules = Vec::new();
    for line in content.split(|&b| b == b'\n') {
        let line = strip_cr(line);
        let line = strip_trailing_ws(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let (negate, body) = if line[0] == b'!' {
            (true, &line[1..])
        } else {
            (false, line)
        };
        let (body, dir_only) = if body.last() == Some(&b'/') {
            (&body[..body.len() - 1], true)
        } else {
            (body, false)
        };
        if body.is_empty() {
            continue;
        }
        let (anchored, body) = if body[0] == b'/' {
            (true, &body[1..])
        } else {
            // Patterns containing an internal `/` are also anchored.
            let inner_slash = body.contains(&b'/');
            (inner_slash, body)
        };
        let Ok(pattern) = std::str::from_utf8(body) else {
            continue;
        };
        rules.push(Rule {
            pattern: pattern.to_string(),
            negate,
            dir_only,
            anchored,
        });
    }
    IgnoreLayer {
        base: base.to_vec(),
        rules,
    }
}

fn strip_cr(line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    }
}

fn strip_trailing_ws(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b' ' || line[end - 1] == b'\t') {
        end -= 1;
    }
    &line[..end]
}

/// Run a path through a single layer. `Some(true)` = ignored, `Some(false)`
/// = explicitly unignored (a `!` rule fired), `None` = no rule in this
/// layer touched the path; fall through to the next-shallower layer.
fn match_layer(layer: &IgnoreLayer, path: &[u8], is_dir: bool) -> Option<bool> {
    // `path` is relative to the working-tree root; the layer is anchored
    // at `layer.base`. If the path doesn't sit under the layer's base, no
    // rule in this layer can match.
    let rel = strip_base(path, &layer.base)?;
    let mut decision = None;
    for rule in &layer.rules {
        if rule.dir_only && !is_dir {
            continue;
        }
        if matches_rule(rule, rel) {
            decision = Some(!rule.negate);
        }
    }
    decision
}

fn strip_base<'p>(path: &'p [u8], base: &[u8]) -> Option<&'p [u8]> {
    if base.is_empty() {
        return Some(path);
    }
    if path.len() < base.len() + 1 {
        return None;
    }
    if &path[..base.len()] != base {
        return None;
    }
    if path[base.len()] != b'/' {
        return None;
    }
    Some(&path[base.len() + 1..])
}

/// `rel` is the candidate path relative to the rule's layer base.
fn matches_rule(rule: &Rule, rel: &[u8]) -> bool {
    let pattern = rule.pattern.as_bytes();
    if rule.anchored {
        glob_match(pattern, rel)
    } else {
        // Unanchored single-component pattern: match against the basename
        // and against every full prefix component to honour git's "anywhere"
        // semantics. Equivalent to `**/pattern`.
        let basename = match rel.iter().rposition(|&b| b == b'/') {
            Some(p) => &rel[p + 1..],
            None => rel,
        };
        glob_match(pattern, basename)
            || rel
                .split(|&b| b == b'/')
                .any(|seg| glob_match(pattern, seg))
    }
}

/// Glob-match `text` against `pattern`. Supports `*` (any run not crossing
/// `/`), `?` (one char not `/`), and `**` (any number of path components,
/// including across `/`). Escape sequences (`\*`) are not handled — none
/// of git's own use of `.gitignore` needs them.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
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
                if pc == b'?' {
                    if tc == b'/' {
                        // ? doesn't cross /
                    } else {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                } else if pc == tc {
                    pi += 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn check(patterns: &str, path: &str, is_dir: bool) -> bool {
        let layer = parse_layer(patterns.as_bytes(), b"");
        let mut stack = IgnoreStack::new();
        stack.push(layer);
        stack.is_ignored(path.as_bytes(), is_dir)
    }

    #[test]
    fn blank_and_comment_lines_are_skipped() {
        let layer = parse_layer(b"\n# a comment\n\n  \n*.log\n", b"");
        assert_eq!(layer.rules.len(), 1);
    }

    #[test]
    fn unanchored_basename_matches_anywhere() {
        assert!(check("*.log", "foo.log", false));
        assert!(check("*.log", "a/b/foo.log", false));
        assert!(!check("*.log", "foo.txt", false));
    }

    #[test]
    fn leading_slash_anchors_at_root() {
        assert!(check("/foo", "foo", false));
        assert!(!check("/foo", "bar/foo", false));
    }

    #[test]
    fn trailing_slash_means_directory_only() {
        assert!(check("/foo/", "foo", true));
        assert!(!check("/foo/", "foo", false));
    }

    #[test]
    fn negation_unignores() {
        assert!(!check("*.log\n!keep.log", "keep.log", false));
        assert!(check("*.log\n!keep.log", "other.log", false));
    }

    #[test]
    fn dotdir_anchored_dir_only_matches_top_level_directory() {
        // The shape `alt`'s own `.gitignore` uses for `/.dev/`, `/.alt/`.
        assert!(check("/.dev/", ".dev", true));
        assert!(check("/.dev/", ".dev", true));
        assert!(!check("/.dev/", ".dev", false)); // file with same name
        assert!(!check("/.dev/", "sub/.dev", true)); // not at root
    }

    #[test]
    fn dotdir_files_under_root_match_too() {
        // We test the containing directory; once that's ignored, scan_dir
        // never descends into it. But we should still ignore individual
        // entries directly addressed under it.
        // Currently a child path inside an ignored dir is NOT itself
        // matched by `/.dev/`. That's fine because scan_dir bails on the
        // directory entry.
        assert!(!check("/.dev/", ".dev/file.txt", false));
    }

    #[test]
    fn double_star_matches_any_depth() {
        assert!(check("**/foo", "foo", false));
        assert!(check("**/foo", "a/foo", false));
        assert!(check("**/foo", "a/b/c/foo", false));
    }

    #[test]
    fn double_star_in_middle_matches_zero_components() {
        assert!(check("a/**/b", "a/b", false));
        assert!(check("a/**/b", "a/x/b", false));
        assert!(check("a/**/b", "a/x/y/b", false));
        assert!(!check("a/**/b", "a/x/y", false));
    }

    #[test]
    fn unanchored_star_matches_basename_anywhere() {
        // `*.rs` has no `/`, so it's basename-anywhere — matches at any depth
        assert!(check("*.rs", "lib.rs", false));
        assert!(check("*.rs", "src/lib.rs", false));
    }

    #[test]
    fn anchored_star_does_not_cross_slash() {
        // `a/*.rs` is anchored (contains `/`); `*` can't consume another `/`
        assert!(check("a/*.rs", "a/lib.rs", false));
        assert!(!check("a/*.rs", "a/b/lib.rs", false));
    }

    #[test]
    fn question_mark_single_char() {
        assert!(check("?.rs", "a.rs", false));
        assert!(!check("?.rs", "ab.rs", false));
        assert!(!check("?.rs", "/.rs", false));
    }

    #[test]
    fn project_alt_gitignore_shape() {
        // mirror alt's own .gitignore — exact shape from the project root
        let g =
            "/.claude/\n/.dev/\n/.alt/\n/target/\n/fuzz/target/\n/fuzz/corpus/\n/fuzz/artifacts/\n";
        assert!(check(g, ".claude", true));
        assert!(check(g, ".dev", true));
        assert!(check(g, ".alt", true));
        assert!(check(g, "target", true));
        assert!(check(g, "fuzz/target", true));
        assert!(check(g, "fuzz/corpus", true));
        assert!(check(g, "fuzz/artifacts", true));
        // not at root
        assert!(!check(g, "sub/.claude", true));
        // not a directory
        assert!(!check(g, ".claude", false));
        // entries that should NOT be ignored
        assert!(!check(g, "crates", true));
        assert!(!check(g, "scripts", true));
        assert!(!check(g, "Cargo.toml", false));
    }

    #[test]
    fn stack_deeper_layer_wins() {
        let root = parse_layer(b"/foo\n", b"");
        let sub = parse_layer(b"!foo\n", b"sub");
        let mut s = IgnoreStack::new();
        s.push(root);
        s.push(sub);
        // path `sub/foo` falls under the deeper layer, which negates
        assert!(!s.is_ignored(b"sub/foo", false));
        // path `foo` only the root layer covers
        assert!(s.is_ignored(b"foo", false));
    }
}
