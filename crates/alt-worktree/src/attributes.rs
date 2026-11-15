//! Git attributes that change how content is stored: which paths are
//! text and how their line endings differ between the object store and the
//! working tree, which carry an `$Id$` keyword, and which go through a
//! filter.
//!
//! The `text`, `eol`, legacy `crlf`, `ident` and `filter` attributes are
//! read, with the macros that can set them (`binary`, and `[attr]`
//! definitions). Where no attribute decides, `core.autocrlf` does.

use std::collections::HashMap;

use crate::glob::glob_match;

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

/// Where an attributes file comes from. A later kind overrides an earlier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
enum Source {
    /// the user's file, `core.attributesFile`
    Global,
    /// a `.gitattributes` file in the tree
    #[default]
    Tree,
    /// the repository's own `info/attributes`
    Info,
}

/// The line-ending settings of the repository's configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EolConfig {
    pub autocrlf: AutoCrlf,
    pub eol: CoreEol,
}

/// `core.autocrlf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoCrlf {
    #[default]
    False,
    True,
    Input,
}

/// `core.eol`. The native line ending is LF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoreEol {
    #[default]
    Native,
    Lf,
    Crlf,
}

impl EolConfig {
    /// Whether text without an `eol` attribute is checked out with CRLF.
    fn text_is_crlf(&self) -> bool {
        match self.autocrlf {
            AutoCrlf::True => true,
            AutoCrlf::Input => false,
            AutoCrlf::False => self.eol == CoreEol::Crlf,
        }
    }
}

/// What the attributes say about storing one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathAttributes {
    pub conversion: Conversion,
    /// `$Id$` is expanded to the blob id on checkout
    pub ident: bool,
    /// the name of the filter driver the content goes through
    pub filter: Option<Vec<u8>>,
}

/// One attributes file.
#[derive(Debug, Clone, Default)]
pub struct Layer {
    source: Source,
    /// directory holding the file, relative to the root, without a
    /// trailing slash; empty for the root
    base: Vec<u8>,
    lines: Vec<Line>,
    macros: Vec<(Vec<u8>, Settings)>,
}

impl Layer {
    /// Parses the `.gitattributes` file found in directory `base`. Macro
    /// definitions count only in the root file, as in git.
    pub fn parse(content: &[u8], base: &[u8]) -> Layer {
        Layer::from(content, base, Source::Tree)
    }

    /// Parses the user's attributes file, which every `.gitattributes`
    /// file overrides.
    pub fn global(content: &[u8]) -> Layer {
        Layer::from(content, b"", Source::Global)
    }

    /// Parses the repository's `info/attributes`, which overrides every
    /// other file.
    pub fn info(content: &[u8]) -> Layer {
        Layer::from(content, b"", Source::Info)
    }

    fn from(content: &[u8], base: &[u8], source: Source) -> Layer {
        let mut layer = Layer {
            source,
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

/// What the attributes say about `path` (relative to the root,
/// `/`-separated) under `layers`: the user's file, the attribute files of
/// the root and of every directory above the path, and the repository's
/// own file, in any order.
pub fn lookup<'a>(
    layers: impl IntoIterator<Item = &'a Layer>,
    path: &[u8],
    config: &EolConfig,
) -> PathAttributes {
    let mut layers: Vec<&Layer> = layers.into_iter().collect();
    // the repository's own file decides first, then the deepest file in
    // the tree, the user's file last; within a file the last line
    layers.sort_by_key(|l| std::cmp::Reverse((l.source, l.base.len())));
    // the first state met for an attribute stands; later files and
    // earlier lines cannot change it
    let mut states: HashMap<&[u8], &State> = HashMap::new();
    for layer in &layers {
        let Some(rel) = below(path, &layer.base) else {
            continue;
        };
        let name = rel.rsplit(|&b| b == b'/').next().unwrap_or(rel);
        for line in layer.lines.iter().rev() {
            let subject = if line.anchored { rel } else { name };
            if glob_match(&line.pattern, subject) {
                fill(&mut states, &line.settings, &layers);
            }
        }
    }
    let state = |name: &[u8]| states.get(name).map(|s| (*s).clone());
    PathAttributes {
        conversion: decide(state(b"text"), state(b"crlf"), state(b"eol"), config),
        ident: state(b"ident") == Some(State::Set),
        filter: match state(b"filter") {
            Some(State::Value(name)) => Some(name),
            _ => None,
        },
    }
}

/// Records the settings of one matching line, last first. A macro that
/// comes out set stands for its definition, which is recorded in turn; one
/// that an earlier-read line has already unset is not expanded.
fn fill<'l>(
    states: &mut HashMap<&'l [u8], &'l State>,
    settings: &'l Settings,
    layers: &[&'l Layer],
) {
    for (attr, state) in settings.iter().rev() {
        if states.contains_key(attr.as_slice()) {
            continue;
        }
        states.insert(attr, state);
        if *state == State::Set {
            fill(states, expand(layers, attr), layers);
        }
    }
}

fn below<'p>(path: &'p [u8], base: &[u8]) -> Option<&'p [u8]> {
    if base.is_empty() {
        return Some(path);
    }
    path.strip_prefix(base)?.strip_prefix(b"/")
}

/// The settings a macro stands for: its last definition in the file of
/// highest precedence that defines it, or the built-in `binary`.
fn expand<'l>(layers: &[&'l Layer], name: &[u8]) -> &'l Settings {
    static BINARY: std::sync::OnceLock<Settings> = std::sync::OnceLock::new();
    let defined = layers.iter().find_map(|layer| {
        let found = layer.macros.iter().rev().find(|(n, _)| n == name);
        found.map(|(_, settings)| settings)
    });
    match (defined, name) {
        (Some(settings), _) => settings,
        (None, b"binary") => BINARY.get_or_init(|| {
            [&b"diff"[..], b"merge", b"text"]
                .iter()
                .map(|n| (n.to_vec(), State::Unset))
                .collect()
        }),
        _ => {
            static NONE: Settings = Vec::new();
            &NONE
        }
    }
}

fn decide(
    text: Option<State>,
    legacy: Option<State>,
    eol: Option<State>,
    config: &EolConfig,
) -> Conversion {
    // `text` (or the older `crlf`): set, unset, `input` or `auto`
    let kind = |state: &Option<State>| match state {
        Some(State::Set) => Some("text"),
        Some(State::Unset) => Some("binary"),
        Some(State::Value(v)) if v == b"input" => Some("input"),
        Some(State::Value(v)) if v == b"auto" => Some("auto"),
        _ => None,
    };
    let crlf = match eol {
        Some(State::Value(v)) if v == b"crlf" => Some(true),
        Some(State::Value(v)) if v == b"lf" => Some(false),
        _ => None,
    };
    let (text_crlf, auto_crlf) = (Conversion::TextCrlf, Conversion::AutoCrlf);
    match (kind(&text).or_else(|| kind(&legacy)), crlf) {
        (Some("binary"), _) => Conversion::Binary,
        (Some("auto"), Some(true)) => auto_crlf,
        (Some("auto"), Some(false)) => Conversion::AutoInput,
        (Some("auto"), None) if config.text_is_crlf() => auto_crlf,
        (Some("auto"), None) => Conversion::AutoInput,
        // `eol` alone marks the path as text
        (_, Some(true)) => text_crlf,
        (_, Some(false)) | (Some("input"), None) => Conversion::TextInput,
        (Some(_), None) if config.text_is_crlf() => text_crlf,
        (Some(_), None) => Conversion::TextInput,
        // no attribute says anything: the configuration decides
        (None, None) => match config.autocrlf {
            AutoCrlf::False => Conversion::Binary,
            AutoCrlf::True => auto_crlf,
            AutoCrlf::Input => Conversion::AutoInput,
        },
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
        lookup(&layers, path.as_bytes(), &EolConfig::default()).conversion
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
    fn the_configuration_decides_where_no_attribute_does() {
        let with = |autocrlf, eol, rules: &str, path: &str| {
            let layers = [Layer::parse(rules.as_bytes(), b"")];
            lookup(&layers, path.as_bytes(), &EolConfig { autocrlf, eol }).conversion
        };
        let (off, native) = (AutoCrlf::False, CoreEol::Native);
        assert_eq!(
            with(AutoCrlf::True, native, "", "a.c"),
            Conversion::AutoCrlf
        );
        assert_eq!(
            with(AutoCrlf::Input, native, "", "a.c"),
            Conversion::AutoInput
        );
        assert_eq!(with(off, CoreEol::Crlf, "", "a.c"), Conversion::Binary);
        // text without an eol attribute follows core.eol, and autocrlf first
        assert_eq!(
            with(off, CoreEol::Crlf, "* text\n", "a.c"),
            Conversion::TextCrlf
        );
        assert_eq!(
            with(off, CoreEol::Lf, "* text\n", "a.c"),
            Conversion::TextInput
        );
        assert_eq!(
            with(AutoCrlf::True, CoreEol::Lf, "* text\n", "a.c"),
            Conversion::TextCrlf
        );
        assert_eq!(
            with(AutoCrlf::Input, CoreEol::Crlf, "* text\n", "a.c"),
            Conversion::TextInput
        );
        assert_eq!(
            with(off, CoreEol::Crlf, "* text=auto\n", "a.c"),
            Conversion::AutoCrlf
        );
        // an attribute still wins over the configuration
        assert_eq!(
            with(AutoCrlf::True, native, "* -text\n", "a.c"),
            Conversion::Binary
        );
        assert_eq!(
            with(AutoCrlf::True, native, "* eol=lf\n", "a.c"),
            Conversion::TextInput
        );
        assert_eq!(
            with(AutoCrlf::True, native, "* text=input\n", "a.c"),
            Conversion::TextInput
        );
    }

    #[test]
    fn the_users_file_is_overridden_and_the_repositorys_own_file_overrides() {
        let layers = [
            Layer::parse(b"*.txt eol=lf ident\n", b""),
            Layer::global(b"*.txt eol=crlf filter=g\n*.md eol=crlf\n[attr]win eol=crlf\n"),
            Layer::parse(b"*.txt -ident\n", b"sub"),
            Layer::info(b"keep.txt eol=crlf\n*.bat win\n"),
        ];
        let at = |path: &str| lookup(&layers, path.as_bytes(), &EolConfig::default());
        assert_eq!(at("a.txt").conversion, Conversion::TextInput);
        assert_eq!(at("a.txt").filter.as_deref(), Some(&b"g"[..]));
        assert!(at("a.txt").ident);
        assert!(!at("sub/a.txt").ident);
        assert_eq!(at("a.md").conversion, Conversion::TextCrlf);
        assert_eq!(at("sub/keep.txt").conversion, Conversion::TextCrlf);
        // a macro from the user's file, used in the repository's own
        assert_eq!(at("run.bat").conversion, Conversion::TextCrlf);
        assert_eq!(
            at("other.c"),
            PathAttributes {
                conversion: Conversion::Binary,
                ident: false,
                filter: None,
            }
        );
    }

    #[test]
    fn a_macro_is_expanded_only_where_it_comes_out_set() {
        // the later line unsets the macro itself: nothing is expanded
        assert_eq!(
            conv(
                &[("", "* eol=crlf\n*.bin binary\n*.bin -binary\n")],
                "a.bin"
            ),
            Conversion::TextCrlf
        );
        assert_eq!(
            conv(&[("", "* eol=crlf\n*.bin binary\n")], "a.bin"),
            Conversion::Binary
        );
        // a macro whose definition names another macro
        let nested = "[attr]raw binary\n[attr]asset raw\n* eol=crlf\n*.dat asset\n";
        assert_eq!(conv(&[("", nested)], "a.dat"), Conversion::Binary);
        // what the line sets after the macro wins over the expansion
        assert_eq!(
            conv(&[("", "*.x binary text eol=crlf\n")], "a.x"),
            Conversion::TextCrlf
        );
        assert_eq!(
            conv(&[("", "*.x text binary\n")], "a.x"),
            Conversion::Binary
        );
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
