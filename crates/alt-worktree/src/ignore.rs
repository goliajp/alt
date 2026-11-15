//! `.gitignore` parsing and matching for working-tree scans.
//!
//! Implements a useful subset of git's ignore-pattern semantics
//! (`gitignore(5)`): enough to handle every shape that appears in real
//! `.gitignore` files at the repository root. Patterns are loaded from one
//! file at a time into an [`IgnoreLayer`]; `scan_dir` stacks layers as it
//! descends, and the deepest layer's last matching rule wins. Below every
//! `.gitignore` sits the user's global excludes file, at git's default
//! location: `$XDG_CONFIG_HOME/git/ignore`, else `$HOME/.config/git/ignore`.
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
//! - `[abc]`, `[a-z]`, `[!a-z]` and `[[:alpha:]]` match one character of
//!   the class; `\x` matches `x` itself.
//!
//! ## Not supported (yet)
//!
//! - A `core.excludesFile` setting that moves the global excludes file
//!   (the default location above is read).
//! - Per-repository `info/exclude`.

use crate::glob::glob_match;

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
    /// the rules that name one path and nothing else, by that path: their
    /// positions in `rules`, in order
    exact: std::collections::HashMap<Vec<u8>, Vec<u32>>,
    /// the positions of all other rules, in order
    general: Vec<u32>,
}

/// A stack of `IgnoreLayer`s — the root layer at index 0, the deepest
/// active layer at the top. `is_ignored` consults the top first; if no
/// layer matches the path, it is not ignored.
#[derive(Debug, Default, Clone)]
pub(crate) struct IgnoreStack {
    layers: Vec<std::sync::Arc<IgnoreLayer>>,
}

impl IgnoreStack {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A stack whose bottom layer is the user's global excludes file, rooted
    /// at the working tree, so every `.gitignore` takes precedence over it.
    pub(crate) fn with_global_excludes() -> std::io::Result<Self> {
        let mut stack = Self::new();
        if let Some(path) = global_excludes_path() {
            match std::fs::read(path) {
                Ok(bytes) => stack.push(parse_layer(&bytes, b"")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(stack)
    }

    pub(crate) fn push(&mut self, layer: IgnoreLayer) {
        self.layers.push(std::sync::Arc::new(layer));
    }

    /// This stack with `layer` on top, for a walk that goes down several
    /// directories at once.
    #[cfg(unix)]
    pub(crate) fn with(&self, layer: std::sync::Arc<IgnoreLayer>) -> Self {
        let mut stack = self.clone();
        stack.layers.push(layer);
        stack
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

/// What the user's global excludes file looks like now, to tell when it
/// has changed; `None` when there is none.
#[cfg(target_os = "linux")]
pub(crate) fn global_excludes_stat() -> Option<crate::snapshot::FileStat> {
    let meta = std::fs::metadata(global_excludes_path()?).ok()?;
    Some(crate::snapshot::FileStat::of(&meta))
}

/// git's global excludes file when `core.excludesFile` is unset. An empty
/// variable counts as unset, as it does for git.
fn global_excludes_path() -> Option<std::path::PathBuf> {
    let set = |k| std::env::var_os(k).filter(|v| !v.is_empty());
    let config = set("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| set("HOME").map(|h| std::path::Path::new(&h).join(".config")))?;
    Some(config.join("git").join("ignore"))
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
    let mut exact: std::collections::HashMap<Vec<u8>, Vec<u32>> = Default::default();
    let mut general = Vec::new();
    for (at, rule) in rules.iter().enumerate() {
        let literal = !rule
            .pattern
            .bytes()
            .any(|b| matches!(b, b'*' | b'?' | b'[' | b'\\'));
        match rule.anchored && literal {
            true => exact
                .entry(rule.pattern.clone().into_bytes())
                .or_default()
                .push(at as u32),
            false => general.push(at as u32),
        }
    }
    IgnoreLayer {
        base: base.to_vec(),
        rules,
        exact,
        general,
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
    let applies = |at: &&u32| !layer.rules[**at as usize].dir_only || is_dir;
    // the last rule that matches decides
    let named = layer
        .exact
        .get(rel)
        .and_then(|at| at.iter().rev().find(applies));
    let general = layer
        .general
        .iter()
        .rev()
        .take_while(|at| named.is_none_or(|n| *at > n))
        .filter(applies)
        .find(|at| matches_rule(&layer.rules[**at as usize], rel));
    general
        .or(named)
        .map(|at| !layer.rules[*at as usize].negate)
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
        // an unanchored pattern matches any component of the path, the
        // last one included: the same as `**/pattern`
        rel.split(|&b| b == b'/')
            .any(|seg| glob_match(pattern, seg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_matching_rule_decides_whatever_its_form() {
        let text = b"/out\n*.log\n!/keep.log\n/keep.log\n!keep.*\n/dir/\n/a/b\n!/a/b\nb\n/x.tmp\n!*.tmp\n/x.tmp\n";
        let layer = parse_layer(text, b"");
        // every rule in order, the last match kept
        let slow = |path: &[u8], is_dir: bool| {
            let mut decision = None;
            for rule in &layer.rules {
                if (!rule.dir_only || is_dir) && matches_rule(rule, path) {
                    decision = Some(!rule.negate);
                }
            }
            decision
        };
        for path in [
            &b"out"[..],
            b"keep.log",
            b"other.log",
            b"dir",
            b"a/b",
            b"a/c",
            b"b",
            b"x.tmp",
            b"y.tmp",
            b"none",
        ] {
            for is_dir in [false, true] {
                assert_eq!(
                    match_layer(&layer, path, is_dir),
                    slow(path, is_dir),
                    "{:?}",
                    path
                );
            }
        }
        assert_eq!(match_layer(&layer, b"out", false), Some(true));
        assert_eq!(match_layer(&layer, b"keep.log", false), Some(false));
        assert_eq!(match_layer(&layer, b"dir", false), None);
        assert_eq!(match_layer(&layer, b"x.tmp", false), Some(true));
        assert_eq!(match_layer(&layer, b"a/b", false), Some(true));
    }

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
        // a rooted directory rule, the shape of `/.alt/` or `/target/`
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
    fn rooted_directory_rules() {
        // a typical project .gitignore: rooted directory-only rules
        let g =
            "/.cache/\n/.dev/\n/.alt/\n/target/\n/fuzz/target/\n/fuzz/corpus/\n/fuzz/artifacts/\n";
        assert!(check(g, ".cache", true));
        assert!(check(g, ".dev", true));
        assert!(check(g, ".alt", true));
        assert!(check(g, "target", true));
        assert!(check(g, "fuzz/target", true));
        assert!(check(g, "fuzz/corpus", true));
        assert!(check(g, "fuzz/artifacts", true));
        // not at root
        assert!(!check(g, "sub/.cache", true));
        // not a directory
        assert!(!check(g, ".cache", false));
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
