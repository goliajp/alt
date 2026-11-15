//! `.gitattributes` line-ending rules: which paths are text, and how their
//! line endings differ between the object store and the working tree.
//!
//! Only the `text`, `eol` and legacy `crlf` attributes are read, plus the
//! macros that can set them (`binary`, and `[attr]` definitions in the root
//! file). A path no rule covers is stored and checked out byte for byte.

use crate::ignore::glob_match;

/// How a path's content is converted, following git's attribute rules on a
/// platform whose native line ending is LF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conversion {
    /// stored and checked out byte for byte
    Binary,
    /// CRLF becomes LF when stored; checked out as stored
    TextInput,
    /// CRLF becomes LF when stored; LF becomes CRLF when checked out
    TextCrlf,
    /// like `TextInput`, for content that looks like text
    AutoInput,
    /// like `TextCrlf`, for content that looks like text
    AutoCrlf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Set,
    Unset,
    Unspecified,
    Value(Vec<u8>),
}

/// attribute settings in the order written
type Settings = Vec<(Vec<u8>, State)>;

#[derive(Debug, Clone)]
struct Line {
    pattern: Vec<u8>,
    /// matched against the whole path below the file's directory rather
    /// than the last component
    anchored: bool,
    settings: Settings,
}

/// One `.gitattributes` file.
#[derive(Debug, Clone, Default)]
pub struct Layer {
    /// directory holding the file, relative to the root, without a
    /// trailing slash; empty for the root
    base: Vec<u8>,
    lines: Vec<Line>,
    macros: Vec<(Vec<u8>, Settings)>,
}

impl Layer {
    /// Parses the file found in directory `base`. Macro definitions count
    /// only in the root file, as in git.
    pub fn parse(content: &[u8], base: &[u8]) -> Layer {
        let mut layer = Layer {
            base: base.to_vec(),
            ..Layer::default()
        };
        for raw in content.split(|&b| b == b'\n') {
            let mut words = raw
                .split(|b| b.is_ascii_whitespace())
                .filter(|w| !w.is_empty());
            let Some(first) = words.next() else { continue };
            if first.starts_with(b"#") {
                continue;
            }
            let settings: Settings = words.map(setting).collect();
            if let Some(name) = first.strip_prefix(b"[attr]") {
                if base.is_empty() {
                    layer.macros.push((name.to_vec(), settings));
                }
                continue;
            }
            // negative patterns are not allowed, and a pattern for a
            // directory never covers the files inside it
            if first.starts_with(b"!") || first.ends_with(b"/") {
                continue;
            }
            let pattern = first.strip_prefix(b"/").unwrap_or(first);
            layer.lines.push(Line {
                pattern: pattern.to_vec(),
                anchored: first.contains(&b'/'),
                settings,
            });
        }
        layer
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.macros.is_empty()
    }
}

fn setting(word: &[u8]) -> (Vec<u8>, State) {
    if let Some(name) = word.strip_prefix(b"-") {
        return (name.to_vec(), State::Unset);
    }
    if let Some(name) = word.strip_prefix(b"!") {
        return (name.to_vec(), State::Unspecified);
    }
    match word.iter().position(|&b| b == b'=') {
        Some(at) => (word[..at].to_vec(), State::Value(word[at + 1..].to_vec())),
        None => (word.to_vec(), State::Set),
    }
}

/// The conversion for `path` (relative to the root, `/`-separated) under
/// `layers`: the attribute files of the root and of every directory above
/// the path, in any order.
pub fn conversion<'a>(layers: impl IntoIterator<Item = &'a Layer>, path: &[u8]) -> Conversion {
    let mut layers: Vec<&Layer> = layers.into_iter().collect();
    if layers.is_empty() {
        return Conversion::Binary;
    }
    // the deepest file decides first, and within a file the last line
    layers.sort_by_key(|l| std::cmp::Reverse(l.base.len()));
    let root = layers.iter().find(|l| l.base.is_empty());
    let (mut text, mut eol, mut crlf) = (None, None, None);
    let mut see = |name: &[u8], state: &State| {
        let slot = match name {
            b"text" => &mut text,
            b"eol" => &mut eol,
            b"crlf" => &mut crlf,
            _ => return,
        };
        if slot.is_none() {
            *slot = Some(state.clone());
        }
    };
    for layer in &layers {
        let Some(rel) = below(path, &layer.base) else {
            continue;
        };
        let name = rel.rsplit(|&b| b == b'/').next().unwrap_or(rel);
        for line in layer.lines.iter().rev() {
            let subject = if line.anchored { rel } else { name };
            if !glob_match(&line.pattern, subject) {
                continue;
            }
            for (attr, state) in line.settings.iter().rev() {
                // a macro that is set stands for its definition
                if *state == State::Set {
                    for (inner, inner_state) in expand(root, attr).iter().rev() {
                        see(inner, inner_state);
                    }
                }
                see(attr, state);
            }
        }
    }
    decide(text, crlf, eol)
}

fn below<'p>(path: &'p [u8], base: &[u8]) -> Option<&'p [u8]> {
    if base.is_empty() {
        return Some(path);
    }
    path.strip_prefix(base)?.strip_prefix(b"/")
}

fn expand<'l>(root: Option<&&'l Layer>, name: &[u8]) -> &'l [(Vec<u8>, State)] {
    static BINARY: std::sync::OnceLock<Settings> = std::sync::OnceLock::new();
    let defined = root.and_then(|layer| {
        layer
            .macros
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, settings)| settings.as_slice())
    });
    match (defined, name) {
        (Some(settings), _) => settings,
        (None, b"binary") => BINARY.get_or_init(|| {
            [&b"diff"[..], b"merge", b"text"]
                .iter()
                .map(|n| (n.to_vec(), State::Unset))
                .collect()
        }),
        _ => &[],
    }
}

fn decide(text: Option<State>, legacy: Option<State>, eol: Option<State>) -> Conversion {
    // `text` (or the older `crlf`): set, unset, `input` or `auto`
    let kind = |state: &Option<State>| match state {
        Some(State::Set) => Some("text"),
        Some(State::Unset) => Some("binary"),
        Some(State::Value(v)) if v == b"input" => Some("text"),
        Some(State::Value(v)) if v == b"auto" => Some("auto"),
        _ => None,
    };
    let crlf = match eol {
        Some(State::Value(v)) if v == b"crlf" => Some(true),
        Some(State::Value(v)) if v == b"lf" => Some(false),
        _ => None,
    };
    match (kind(&text).or_else(|| kind(&legacy)), crlf) {
        (Some("binary"), _) | (None, None) => Conversion::Binary,
        (Some("auto"), Some(true)) => Conversion::AutoCrlf,
        (Some("auto"), _) => Conversion::AutoInput,
        // `eol` alone marks the path as text
        (_, Some(true)) => Conversion::TextCrlf,
        (_, _) => Conversion::TextInput,
    }
}

#[derive(Default)]
struct Stats {
    nul: usize,
    lone_cr: usize,
    lone_lf: usize,
    crlf: usize,
    printable: usize,
    nonprintable: usize,
}

impl Stats {
    fn of(data: &[u8]) -> Stats {
        let mut s = Stats::default();
        let mut i = 0;
        while i < data.len() {
            let c = data[i];
            i += 1;
            match c {
                b'\r' if data.get(i) == Some(&b'\n') => {
                    s.crlf += 1;
                    i += 1;
                }
                b'\r' => s.lone_cr += 1,
                b'\n' => s.lone_lf += 1,
                0x7f => s.nonprintable += 1,
                0x08 | b'\t' | 0x1b | 0x0c => s.printable += 1,
                0 => {
                    s.nul += 1;
                    s.nonprintable += 1;
                }
                c if c < 32 => s.nonprintable += 1,
                _ => s.printable += 1,
            }
        }
        // a trailing ctrl-z end-of-file mark is not held against the text
        if data.last() == Some(&0x1a) {
            s.nonprintable -= 1;
        }
        s
    }

    fn binary(&self) -> bool {
        self.lone_cr > 0 || self.nul > 0 || (self.printable >> 7) < self.nonprintable
    }
}

/// Whether stored content is text that already has CRLF line endings; an
/// `auto` path stored that way is left alone when staged again.
pub fn stored_with_crlf(blob: &[u8]) -> bool {
    if !blob.contains(&b'\r') {
        return false;
    }
    let s = Stats::of(blob);
    !s.binary() && s.crlf > 0
}

/// Working-tree content as it is stored, or `None` when it is stored as is.
/// `stored_crlf` says whether the path's staged content
/// [is text with CRLF](stored_with_crlf); it is only asked for `auto` paths.
pub fn to_store(
    conv: Conversion,
    content: &[u8],
    stored_crlf: impl FnOnce() -> bool,
) -> Option<Vec<u8>> {
    if conv == Conversion::Binary || !content.contains(&b'\r') {
        return None;
    }
    let s = Stats::of(content);
    if s.crlf == 0 {
        return None;
    }
    let auto = matches!(conv, Conversion::AutoInput | Conversion::AutoCrlf);
    if auto && (s.binary() || stored_crlf()) {
        return None;
    }
    let mut out = Vec::with_capacity(content.len());
    for (i, &c) in content.iter().enumerate() {
        if c != b'\r' || content.get(i + 1) != Some(&b'\n') {
            out.push(c);
        }
    }
    Some(out)
}

/// Stored content as it is checked out, or `None` when it is written as is.
pub fn to_worktree(conv: Conversion, blob: &[u8]) -> Option<Vec<u8>> {
    let auto = match conv {
        Conversion::TextCrlf => false,
        Conversion::AutoCrlf => true,
        _ => return None,
    };
    let s = Stats::of(blob);
    if s.lone_lf == 0 || (auto && (s.lone_cr > 0 || s.crlf > 0 || s.binary())) {
        return None;
    }
    let mut out = Vec::with_capacity(blob.len() + s.lone_lf);
    for (i, &c) in blob.iter().enumerate() {
        if c == b'\n' && (i == 0 || blob[i - 1] != b'\r') {
            out.push(b'\r');
        }
        out.push(c);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(files: &[(&str, &str)], path: &str) -> Conversion {
        let layers: Vec<Layer> = files
            .iter()
            .map(|(base, text)| Layer::parse(text.as_bytes(), base.as_bytes()))
            .collect();
        conversion(&layers, path.as_bytes())
    }

    #[test]
    fn eol_and_text_decide_the_conversion() {
        let root = "*.bat text eol=crlf\n*.sh eol=lf\n*.txt text\n*.md text=auto\n\
                    *.cs text=auto eol=crlf\n*.png -text eol=crlf\n*.in text=input\n";
        let at = |path| conv(&[("", root)], path);
        assert_eq!(at("a/b/run.bat"), Conversion::TextCrlf);
        assert_eq!(at("run.sh"), Conversion::TextInput);
        assert_eq!(at("notes.txt"), Conversion::TextInput);
        assert_eq!(at("README.md"), Conversion::AutoInput);
        assert_eq!(at("x.cs"), Conversion::AutoCrlf);
        assert_eq!(at("logo.png"), Conversion::Binary);
        assert_eq!(at("x.in"), Conversion::TextInput);
        assert_eq!(
            conv(
                &[("", "* crlf\n*.x text=input eol=crlf\n*.y !text\n")],
                "a.x"
            ),
            Conversion::TextCrlf
        );
        assert_eq!(
            conv(&[("", "* -crlf\n*.y !text eol=crlf\n")], "a.y"),
            Conversion::Binary
        );
        assert_eq!(at("other.c"), Conversion::Binary);
    }

    #[test]
    fn later_lines_and_deeper_files_win() {
        assert_eq!(
            conv(&[("", "* eol=crlf\n*.sh eol=lf\n")], "x.sh"),
            Conversion::TextInput
        );
        assert_eq!(
            conv(&[("", "*.sh eol=lf\n* eol=crlf\n")], "x.sh"),
            Conversion::TextCrlf
        );
        let files = [("", "*.txt eol=crlf\n"), ("sub", "*.txt eol=lf\n")];
        assert_eq!(conv(&files, "sub/deep/a.txt"), Conversion::TextInput);
        assert_eq!(conv(&files, "a.txt"), Conversion::TextCrlf);
        assert_eq!(conv(&files, "subway/a.txt"), Conversion::TextCrlf);
        // a deeper file that says nothing about eol keeps the root's rule
        let files = [("", "*.txt eol=crlf\n"), ("sub", "*.txt -diff\n")];
        assert_eq!(conv(&files, "sub/a.txt"), Conversion::TextCrlf);
    }

    #[test]
    fn patterns_follow_the_attribute_rules() {
        let root = "/top.txt eol=crlf\ndocs/*.txt eol=crlf\nbuild/ eol=crlf\n\
                    !neg.txt eol=crlf\n*.[ch] eol=crlf\n";
        let at = |path| conv(&[("", root)], path);
        assert_eq!(at("top.txt"), Conversion::TextCrlf);
        assert_eq!(at("sub/top.txt"), Conversion::Binary);
        assert_eq!(at("docs/a.txt"), Conversion::TextCrlf);
        assert_eq!(at("docs/deep/a.txt"), Conversion::Binary);
        assert_eq!(at("build/out.txt"), Conversion::Binary);
        assert_eq!(at("neg.txt"), Conversion::Binary);
        assert_eq!(at("src/main.c"), Conversion::TextCrlf);
    }

    #[test]
    fn macros_expand_where_they_are_set() {
        assert_eq!(
            conv(&[("", "* eol=crlf\n*.bin binary\n")], "a.bin"),
            Conversion::Binary
        );
        let root = "[attr]win text eol=crlf\n*.bat win\n*.cmd win -text\n";
        assert_eq!(conv(&[("", root)], "a.bat"), Conversion::TextCrlf);
        assert_eq!(conv(&[("", root)], "a.cmd"), Conversion::Binary);
        // a macro defined below the root is not a macro
        let files = [("", "\n"), ("sub", "[attr]win eol=crlf\n*.bat win\n")];
        assert_eq!(conv(&files, "sub/a.bat"), Conversion::Binary);
    }

    #[test]
    fn checkout_adds_cr_and_staging_removes_it() {
        let crlf = Conversion::TextCrlf;
        assert_eq!(
            to_worktree(crlf, b"a\nb\r\nc").unwrap(),
            b"a\r\nb\r\nc".to_vec()
        );
        assert_eq!(to_worktree(crlf, b"no newline"), None);
        assert_eq!(to_worktree(Conversion::TextInput, b"a\n"), None);
        assert_eq!(to_worktree(Conversion::Binary, b"a\n"), None);
        // auto leaves alone what already has a CR, or is not text
        assert_eq!(to_worktree(Conversion::AutoCrlf, b"a\nb\r\n"), None);
        assert_eq!(to_worktree(Conversion::AutoCrlf, b"a\n\0"), None);
        assert_eq!(
            to_worktree(Conversion::AutoCrlf, b"a\n").unwrap(),
            b"a\r\n".to_vec()
        );

        let no = || false;
        assert_eq!(
            to_store(crlf, b"a\r\nb\rc\r\n", no).unwrap(),
            b"a\nb\rc\n".to_vec()
        );
        assert_eq!(to_store(crlf, b"a\nb\n", no), None);
        assert_eq!(to_store(Conversion::Binary, b"a\r\n", no), None);
        assert_eq!(
            to_store(Conversion::TextInput, b"a\r\n", no).unwrap(),
            b"a\n".to_vec()
        );
        assert_eq!(
            to_store(Conversion::AutoInput, b"a\r\n", no).unwrap(),
            b"a\n".to_vec()
        );
        assert_eq!(to_store(Conversion::AutoInput, b"a\r\n\0", no), None);
        assert_eq!(to_store(Conversion::AutoInput, b"a\r\n", || true), None);
        assert!(stored_with_crlf(b"x\r\ny\n"));
        assert!(!stored_with_crlf(b"x\ny\n"));
        assert!(!stored_with_crlf(b"x\r\n\0"));
    }
}
