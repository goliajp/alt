//! alt-CI workflow.toml: schema-tier types + parser + linter.
//!
//! Pure logic: zero business state, zero I/O — bytes in, diagnostics and a
//! `Workflow` struct out. Reused by `alt ci validate` (the M15 starting
//! point) and by every future segment that takes a workflow tree (runner,
//! scheduler, result cache).
//!
//! ## Scope (M15/W47, design/ci.md §2.1)
//!
//! A purpose-built TOML subset, not a general parser — only the shapes
//! `[trigger]` / `[[step]]` / `[artifacts]` need: strings, string
//! arrays, inline tables, bare keys, comments. No multi-line strings,
//! no inline arrays-of-arrays, no datetimes. If a future workflow
//! field needs more, the parser grows; for now it stays the size of
//! the schema. Reuses no `toml` crate (alt project keeps zero-dep)
//! and stays self-contained from alt-diff::structured (which has a
//! private TOML walker but does not emit line:col on parse errors —
//! the `alt diff` consumer doesn't need them).
//!
//! ## Diagnostics
//!
//! Every error carries the byte offset where it was detected; the
//! caller resolves it to `(line, col)` against the source text via
//! [`Position::resolve`]. Severity is `error` (schema break — exit
//! non-zero) or `warning` (recognised but unimplemented in M15 — exit
//! 0 with the message printed). The `alt ci validate` CLI prints
//! `<path>:<line>:<col>: <severity>: <message>` per diagnostic and
//! also emits a JSON line per diag under `--json` (one record per
//! line, no enclosing array — line-delimited, the same shape the
//! W23 access log uses).

use std::collections::BTreeMap;

/// One workflow definition (`.alt/ci/<name>/workflow.toml`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Workflow {
    pub trigger: Trigger,
    pub steps: Vec<Step>,
    pub artifacts: BTreeMap<String, String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub ref_pattern: Option<String>,
    pub on: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Step {
    pub name: String,
    pub script: String,
    pub agent: Option<String>,
    pub needs: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// One diagnostic emitted by [`parse_workflow`] or [`lint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WfDiag {
    pub at: Position,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// Source position. Byte offset is what the parser actually tracks;
/// `resolve` produces 1-based `(line, col)` against the original text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub byte: usize,
}

impl Position {
    pub fn at(byte: usize) -> Self {
        Self { byte }
    }

    /// Convert byte offset to 1-based (line, col).
    pub fn resolve(self, src: &str) -> (usize, usize) {
        let mut line = 1usize;
        let mut col = 1usize;
        for (i, c) in src.char_indices() {
            if i >= self.byte {
                return (line, col);
            }
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

/// Parse and schema-validate one workflow.toml. The first phase
/// (`parse_toml_subset`) tokenises into a (key path → value) shape;
/// the second (`assemble`) lifts those shapes into a [`Workflow`].
/// Both phases emit diagnostics; assembly continues past recoverable
/// errors so the caller sees every issue in one pass.
pub fn parse_workflow(src: &str) -> (Workflow, Vec<WfDiag>) {
    let mut diags = Vec::new();
    let (tree, parse_diags) = parse_toml_subset(src);
    diags.extend(parse_diags);
    let wf = assemble(tree, &mut diags);
    (wf, diags)
}

/// Run semantic checks on a fully-assembled workflow: step DAG has no
/// cycle, every `needs` entry references a defined step, no duplicate
/// step names. Field-level checks (required strings, illegal chars)
/// already happen during assembly; lint is for whole-graph properties.
pub fn lint(wf: &Workflow) -> Vec<WfDiag> {
    let mut diags = Vec::new();
    // Duplicate step names.
    let mut seen = std::collections::HashSet::new();
    for step in &wf.steps {
        if !seen.insert(step.name.as_str()) {
            diags.push(WfDiag {
                at: Position::at(0),
                severity: Severity::Error,
                message: format!("duplicate step name \"{}\"", step.name),
            });
        }
    }
    // needs references a defined step.
    let names: std::collections::HashSet<&str> = wf.steps.iter().map(|s| s.name.as_str()).collect();
    for step in &wf.steps {
        for need in &step.needs {
            if !names.contains(need.as_str()) {
                diags.push(WfDiag {
                    at: Position::at(0),
                    severity: Severity::Error,
                    message: format!("step \"{}\" needs undefined step \"{}\"", step.name, need),
                });
            }
        }
    }
    // Cycle detection (DFS with white/gray/black colouring).
    let mut by_name: BTreeMap<&str, &Step> = BTreeMap::new();
    for s in &wf.steps {
        by_name.insert(s.name.as_str(), s);
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }
    let mut color: BTreeMap<&str, Color> = by_name.keys().map(|k| (*k, Color::White)).collect();
    fn dfs<'a>(
        name: &'a str,
        by_name: &BTreeMap<&'a str, &'a Step>,
        color: &mut BTreeMap<&'a str, Color>,
        diags: &mut Vec<WfDiag>,
    ) {
        if color.get(name) != Some(&Color::White) {
            return;
        }
        color.insert(name, Color::Gray);
        if let Some(step) = by_name.get(name) {
            for need in &step.needs {
                let need_s: &str = need.as_str();
                match color.get(need_s) {
                    Some(Color::Gray) => {
                        diags.push(WfDiag {
                            at: Position::at(0),
                            severity: Severity::Error,
                            message: format!(
                                "step cycle detected: \"{name}\" reaches \"{need_s}\""
                            ),
                        });
                    }
                    Some(Color::White) => dfs(need_s, by_name, color, diags),
                    _ => {}
                }
            }
        }
        color.insert(name, Color::Black);
    }
    let names: Vec<&str> = by_name.keys().copied().collect();
    for name in names {
        dfs(name, &by_name, &mut color, &mut diags);
    }
    diags
}

// ── lifting: (key path → value) → Workflow ─────────────────────────

/// One leaf-level TOML value we accept in workflow.toml. Nested
/// objects collapse into key paths in the flat tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TomlVal {
    Str(String),
    Arr(Vec<String>),
}

/// One row of the parsed tree: `[a, b, c]` → value, with the byte
/// offset of the key (so a diag can point at the right column).
#[derive(Debug, Clone)]
struct TomlRow {
    path: Vec<TomlSeg>,
    value: TomlVal,
    at: Position,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TomlSeg {
    /// A plain key segment (e.g. `name`).
    Key(String),
    /// One occurrence of an array-of-tables (e.g. `[[step]]`); the
    /// usize is the 0-based index of this occurrence among siblings
    /// at the same prefix.
    ArrIdx(String, usize),
}

fn assemble(rows: Vec<TomlRow>, diags: &mut Vec<WfDiag>) -> Workflow {
    let mut wf = Workflow::default();
    let mut step_by_idx: BTreeMap<usize, Step> = BTreeMap::new();

    for row in rows {
        let path = row.path;
        let val = row.value;
        let at = row.at;
        match path.as_slice() {
            [TomlSeg::Key(ns), TomlSeg::Key(field)] if ns == "trigger" => {
                match (field.as_str(), &val) {
                    ("ref_pattern", TomlVal::Str(s)) => wf.trigger.ref_pattern = Some(s.clone()),
                    ("on", TomlVal::Arr(items)) => {
                        for it in items {
                            if it != "push" {
                                diags.push(WfDiag {
                                    at,
                                    severity: Severity::Warning,
                                    message: format!(
                                        "trigger.on event \"{it}\" not implemented in M15 (only \"push\" runs)"
                                    ),
                                });
                            }
                            wf.trigger.on.push(it.clone());
                        }
                    }
                    (other, _) => diags.push(WfDiag {
                        at,
                        severity: Severity::Error,
                        message: format!("unknown or wrong-type field [trigger].{other}"),
                    }),
                }
            }
            [TomlSeg::Key(ns), TomlSeg::Key(key)] if ns == "artifacts" => match val {
                TomlVal::Str(s) => {
                    wf.artifacts.insert(key.clone(), s);
                }
                _ => diags.push(WfDiag {
                    at,
                    severity: Severity::Error,
                    message: format!(
                        "[artifacts].{key} must be a string path (got non-string value)"
                    ),
                }),
            },
            [TomlSeg::ArrIdx(ns, i), TomlSeg::Key(field)] if ns == "step" => {
                let step = step_by_idx.entry(*i).or_default();
                match (field.as_str(), val) {
                    ("name", TomlVal::Str(s)) => step.name = s,
                    ("script", TomlVal::Str(s)) => step.script = s,
                    ("agent", TomlVal::Str(s)) => step.agent = Some(s),
                    ("needs", TomlVal::Arr(items)) => step.needs = items,
                    (other, _) => diags.push(WfDiag {
                        at,
                        severity: Severity::Error,
                        message: format!("unknown or wrong-type field [[step]].{other}"),
                    }),
                }
            }
            [
                TomlSeg::ArrIdx(ns, i),
                TomlSeg::Key(field),
                TomlSeg::Key(env_key),
            ] if ns == "step" && field == "env" => {
                let step = step_by_idx.entry(*i).or_default();
                match val {
                    TomlVal::Str(s) => {
                        step.env.insert(env_key.clone(), s);
                    }
                    _ => diags.push(WfDiag {
                        at,
                        severity: Severity::Error,
                        message: format!(
                            "step.env.{env_key} must be a string (got non-string value)"
                        ),
                    }),
                }
            }
            _ => {
                let pretty = display_path(&path);
                diags.push(WfDiag {
                    at,
                    severity: Severity::Error,
                    message: format!("unknown table or key {pretty}"),
                });
            }
        }
    }

    for (_, step) in step_by_idx {
        if step.name.is_empty() {
            diags.push(WfDiag {
                at: Position::at(0),
                severity: Severity::Error,
                message: "[[step]] missing required field `name`".to_owned(),
            });
        }
        if step.script.is_empty() {
            diags.push(WfDiag {
                at: Position::at(0),
                severity: Severity::Error,
                message: format!("step \"{}\" missing required field `script`", step.name),
            });
        }
        wf.steps.push(step);
    }

    wf
}

fn display_path(path: &[TomlSeg]) -> String {
    let mut out = String::new();
    for (i, seg) in path.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        match seg {
            TomlSeg::Key(k) => out.push_str(k),
            TomlSeg::ArrIdx(k, _) => out.push_str(k),
        }
    }
    out
}

// ── parser: workflow.toml subset → flat (path, value) rows ────────

/// Walk the source character by character, producing one [`TomlRow`]
/// per `key = value` line and reshuffling `[section]` / `[[array]]`
/// headers into the current path prefix. Errors during the lexical
/// walk (unterminated string, unknown value, dangling `=`) become
/// [`Severity::Error`] diagnostics; recovery skips to the next
/// newline so the rest of the file is still validated.
fn parse_toml_subset(src: &str) -> (Vec<TomlRow>, Vec<WfDiag>) {
    let mut p = P {
        src: src.as_bytes(),
        at: 0,
    };
    let mut rows = Vec::new();
    let mut diags = Vec::new();
    let mut prefix: Vec<TomlSeg> = Vec::new();
    let mut arr_counts: BTreeMap<String, usize> = BTreeMap::new();

    while p.at < p.src.len() {
        p.skip_blanks_and_comments();
        if p.at >= p.src.len() {
            break;
        }
        match p.peek() {
            Some(b'[') => {
                let header_at = p.at;
                let is_array = p.src.get(p.at + 1) == Some(&b'[');
                p.at += if is_array { 2 } else { 1 };
                p.skip_inline_ws();
                let key = match p.read_bare_or_quoted_key() {
                    Some(k) => k,
                    None => {
                        diags.push(WfDiag {
                            at: Position::at(header_at),
                            severity: Severity::Error,
                            message: "expected key inside section header".to_owned(),
                        });
                        p.skip_to_eol();
                        continue;
                    }
                };
                p.skip_inline_ws();
                if p.peek() != Some(b']') {
                    diags.push(WfDiag {
                        at: Position::at(p.at),
                        severity: Severity::Error,
                        message: "expected `]` to close section header".to_owned(),
                    });
                    p.skip_to_eol();
                    continue;
                }
                p.at += 1;
                if is_array {
                    if p.peek() != Some(b']') {
                        diags.push(WfDiag {
                            at: Position::at(p.at),
                            severity: Severity::Error,
                            message: "expected `]]` to close array-of-tables header".to_owned(),
                        });
                        p.skip_to_eol();
                        continue;
                    }
                    p.at += 1;
                }
                p.skip_to_eol();
                prefix.clear();
                if is_array {
                    let idx = arr_counts.entry(key.clone()).or_insert(0);
                    let here = *idx;
                    *idx += 1;
                    prefix.push(TomlSeg::ArrIdx(key, here));
                } else {
                    prefix.push(TomlSeg::Key(key));
                }
            }
            Some(_) => {
                let key_at = p.at;
                let key = match p.read_bare_or_quoted_key() {
                    Some(k) => k,
                    None => {
                        diags.push(WfDiag {
                            at: Position::at(key_at),
                            severity: Severity::Error,
                            message: "expected key".to_owned(),
                        });
                        p.skip_to_eol();
                        continue;
                    }
                };
                // dotted key: `a.b = ...` collapses into nested
                // segments under the current prefix.
                let mut tail: Vec<TomlSeg> = vec![TomlSeg::Key(key)];
                p.skip_inline_ws();
                while p.peek() == Some(b'.') {
                    p.at += 1;
                    p.skip_inline_ws();
                    match p.read_bare_or_quoted_key() {
                        Some(k) => tail.push(TomlSeg::Key(k)),
                        None => {
                            diags.push(WfDiag {
                                at: Position::at(p.at),
                                severity: Severity::Error,
                                message: "expected key after `.`".to_owned(),
                            });
                            p.skip_to_eol();
                            break;
                        }
                    }
                    p.skip_inline_ws();
                }
                p.skip_inline_ws();
                if p.peek() != Some(b'=') {
                    diags.push(WfDiag {
                        at: Position::at(p.at),
                        severity: Severity::Error,
                        message: "expected `=` after key".to_owned(),
                    });
                    p.skip_to_eol();
                    continue;
                }
                p.at += 1;
                p.skip_inline_ws();
                let value_at = p.at;
                let value = match p.read_value() {
                    Ok(v) => v,
                    Err(msg) => {
                        diags.push(WfDiag {
                            at: Position::at(value_at),
                            severity: Severity::Error,
                            message: msg,
                        });
                        p.skip_to_eol();
                        continue;
                    }
                };
                p.skip_to_eol();
                let mut full = prefix.clone();
                full.extend(tail);
                rows.push(TomlRow {
                    path: full,
                    value,
                    at: Position::at(key_at),
                });
            }
            None => break,
        }
    }

    (rows, diags)
}

struct P<'a> {
    src: &'a [u8],
    at: usize,
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.at).copied()
    }

    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.at += 1;
        }
    }

    fn skip_blanks_and_comments(&mut self) {
        loop {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.at += 1;
            }
            if self.peek() == Some(b'#') {
                while let Some(c) = self.peek() {
                    self.at += 1;
                    if c == b'\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn skip_to_eol(&mut self) {
        while let Some(c) = self.peek() {
            self.at += 1;
            if c == b'\n' {
                return;
            }
        }
    }

    fn read_bare_or_quoted_key(&mut self) -> Option<String> {
        match self.peek()? {
            b'"' => self.read_quoted_string().ok(),
            c if is_bare_key_byte(c) => {
                let start = self.at;
                while let Some(c) = self.peek() {
                    if is_bare_key_byte(c) {
                        self.at += 1;
                    } else {
                        break;
                    }
                }
                Some(
                    std::str::from_utf8(&self.src[start..self.at])
                        .ok()?
                        .to_owned(),
                )
            }
            _ => None,
        }
    }

    fn read_value(&mut self) -> Result<TomlVal, String> {
        match self.peek() {
            Some(b'"') => self.read_quoted_string().map(TomlVal::Str),
            Some(b'[') => self.read_string_array().map(TomlVal::Arr),
            Some(_) => Err("unsupported value (only strings and string arrays in M15)".to_owned()),
            None => Err("unexpected end of file in value".to_owned()),
        }
    }

    fn read_quoted_string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err("expected `\"`".to_owned());
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated string".to_owned()),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    match self.peek() {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'n') => out.push('\n'),
                        Some(b't') => out.push('\t'),
                        Some(b'r') => out.push('\r'),
                        Some(c) => {
                            return Err(format!("unrecognised escape `\\{}`", char::from(c)));
                        }
                        None => return Err("unterminated escape in string".to_owned()),
                    }
                    self.at += 1;
                }
                Some(b'\n') => return Err("multi-line strings not supported in M15".to_owned()),
                Some(c) => {
                    out.push(char::from(c));
                    self.at += 1;
                }
            }
        }
    }

    fn read_string_array(&mut self) -> Result<Vec<String>, String> {
        if self.peek() != Some(b'[') {
            return Err("expected `[`".to_owned());
        }
        self.at += 1;
        let mut out = Vec::new();
        loop {
            self.skip_array_ws();
            match self.peek() {
                None => return Err("unterminated array".to_owned()),
                Some(b']') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'"') => {
                    out.push(self.read_quoted_string()?);
                    self.skip_array_ws();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {}
                        _ => {
                            return Err("expected `,` or `]` after array element".to_owned());
                        }
                    }
                }
                _ => return Err("string-only arrays in M15".to_owned()),
            }
        }
    }

    fn skip_array_ws(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.at += 1,
                Some(b'#') => {
                    while let Some(c) = self.peek() {
                        self.at += 1;
                        if c == b'\n' {
                            break;
                        }
                    }
                }
                _ => return,
            }
        }
    }
}

fn is_bare_key_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> Workflow {
        let (wf, diags) = parse_workflow(src);
        assert!(
            diags.is_empty(),
            "expected clean parse, got diags: {diags:?}"
        );
        let lint_diags = lint(&wf);
        assert!(
            lint_diags.is_empty(),
            "expected clean lint, got diags: {lint_diags:?}"
        );
        wf
    }

    fn errs(src: &str) -> Vec<WfDiag> {
        let (wf, mut diags) = parse_workflow(src);
        diags.extend(lint(&wf));
        assert!(
            diags.iter().any(|d| d.severity == Severity::Error),
            "expected at least one error: {diags:?}"
        );
        diags
    }

    #[test]
    fn parses_well_formed_workflow() {
        let src = r#"
[trigger]
ref_pattern = "refs/heads/main"
on = ["push"]

[[step]]
name = "build"
script = "scripts/build.sh"
agent = "ci-runner-linux-x86_64"

[[step]]
name = "test"
script = "scripts/test.sh"
needs = ["build"]
env.RUST_BACKTRACE = "1"

[artifacts]
build_out = "target/release/alt"
"#;
        let wf = ok(src);
        assert_eq!(wf.trigger.ref_pattern.as_deref(), Some("refs/heads/main"));
        assert_eq!(wf.trigger.on, vec!["push".to_owned()]);
        assert_eq!(wf.steps.len(), 2);
        assert_eq!(wf.steps[0].name, "build");
        assert_eq!(wf.steps[0].script, "scripts/build.sh");
        assert_eq!(wf.steps[0].agent.as_deref(), Some("ci-runner-linux-x86_64"));
        assert_eq!(wf.steps[1].name, "test");
        assert_eq!(wf.steps[1].needs, vec!["build".to_owned()]);
        assert_eq!(wf.steps[1].env.get("RUST_BACKTRACE").unwrap(), "1");
        assert_eq!(wf.artifacts.get("build_out").unwrap(), "target/release/alt");
    }

    #[test]
    fn rejects_needs_referencing_undefined_step() {
        let src = r#"
[[step]]
name = "build"
script = "b.sh"

[[step]]
name = "test"
script = "t.sh"
needs = ["nonexistent"]
"#;
        let diags = errs(src);
        assert!(diags.iter().any(|d| d.message.contains("undefined step")));
    }

    #[test]
    fn rejects_duplicate_step_names() {
        let src = r#"
[[step]]
name = "build"
script = "a.sh"

[[step]]
name = "build"
script = "b.sh"
"#;
        let diags = errs(src);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("duplicate step name"))
        );
    }

    #[test]
    fn rejects_step_cycle() {
        let src = r#"
[[step]]
name = "a"
script = "a.sh"
needs = ["b"]

[[step]]
name = "b"
script = "b.sh"
needs = ["a"]
"#;
        let diags = errs(src);
        assert!(
            diags.iter().any(|d| d.message.contains("cycle")),
            "expected cycle diag: {diags:?}"
        );
    }

    #[test]
    fn rejects_missing_required_field() {
        let src = r#"
[[step]]
name = "build"
"#;
        let diags = errs(src);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("missing required field `script`")),
            "expected missing-script diag: {diags:?}"
        );
    }

    #[test]
    fn rejects_unknown_field_under_known_section() {
        let src = r#"
[trigger]
ref_pattern = "main"
unknown_field = "x"
"#;
        let diags = errs(src);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("unknown or wrong-type field")),
            "expected unknown-field diag: {diags:?}"
        );
    }

    #[test]
    fn warning_on_unknown_trigger_event() {
        let src = r#"
[trigger]
on = ["push", "tag"]
"#;
        let (_wf, diags) = parse_workflow(src);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("\"tag\"")),
            "expected warning for unknown trigger event: {diags:?}"
        );
    }

    #[test]
    fn position_resolves_to_line_col() {
        let src = "line1\nline2\nbad";
        // Byte 12 is 'b' in "bad" — line 3, col 1.
        let (line, col) = Position::at(12).resolve(src);
        assert_eq!((line, col), (3, 1));
        // Mid-line offset.
        let (line, col) = Position::at(8).resolve(src);
        assert_eq!((line, col), (2, 3));
    }

    #[test]
    fn rejects_unterminated_string() {
        let src = "[trigger]\nref_pattern = \"unterm\n";
        let (_, diags) = parse_toml_subset(src);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("multi-line") || d.message.contains("unterminated")),
            "expected unterminated/multiline diag: {diags:?}"
        );
    }

    #[test]
    fn rejects_unknown_section() {
        let src = "[oops]\nx = \"y\"\n";
        let diags = errs(src);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("unknown table or key")),
            "expected unknown-section diag: {diags:?}"
        );
    }
}
