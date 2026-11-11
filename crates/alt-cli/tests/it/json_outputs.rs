//! The exact bytes every `--json` command prints, held to transcripts stored
//! under `tests/fixtures/cli/`. A transcript is the commands of one scene with
//! their exit codes and both output streams; values that differ from run to
//! run (object ids, times, the scene's own directory) are replaced by stable
//! placeholders, everything else is compared byte for byte.
//! `ALT_JSON_RECORD=1` rewrites the stored transcripts.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod conflicts;
mod failures;
mod history;
mod receipts;
mod sharing;
mod state_files;
mod tasks;

struct Record {
    cwd: String,
    args: Vec<String>,
    code: Option<i32>,
    streams: Option<(Vec<u8>, Vec<u8>)>,
}

pub struct Scene {
    name: &'static str,
    dir: alt_os::temp::TempDir,
    cwd: String,
    env: Vec<(String, String)>,
    records: Vec<Record>,
    volatile: Vec<&'static str>,
    between: Vec<(&'static str, &'static str)>,
    paths: Vec<(String, String)>,
}

impl Scene {
    pub fn new(name: &'static str) -> Self {
        let dir = alt_os::temp::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("home")).unwrap();
        let root = dir.path().to_str().unwrap().to_owned();
        Scene {
            name,
            dir,
            cwd: String::new(),
            env: Vec::new(),
            records: Vec::new(),
            volatile: Vec::new(),
            between: Vec::new(),
            paths: vec![(root, "<root>".into())],
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root().join(&self.cwd).join(relative)
    }

    pub fn text(&self, relative: &str) -> String {
        self.path(relative).to_str().unwrap().to_owned()
    }

    /// Later commands run in this directory, relative to the scene root.
    pub fn cd(&mut self, relative: &str) {
        self.cwd = relative.to_owned();
        std::fs::create_dir_all(self.path("")).unwrap();
    }

    pub fn env(&mut self, key: &str, value: &str) {
        self.env.retain(|(k, _)| k != key);
        self.env.push((key.into(), value.into()));
    }

    /// A text that differs from run to run, such as a server address.
    pub fn alias(&mut self, text: &str, name: &str) {
        self.paths.push((text.to_owned(), format!("<{name}>")));
    }

    /// JSON members whose values are replaced whole.
    pub fn volatile(&mut self, keys: &[&'static str]) {
        self.volatile.extend_from_slice(keys);
    }

    /// Human-readable text whose part between two markers is replaced.
    pub fn volatile_between(&mut self, before: &'static str, after: &'static str) {
        self.between.push((before, after));
    }

    pub fn write(&self, relative: &str, contents: impl AsRef<[u8]>) {
        let path = self.path(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_alt"));
        command
            .current_dir(self.path(""))
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", self.root().join("home"))
            .env("USER", "json-test")
            .env("TZ", "UTC")
            .env("ALT_NO_DAEMON", "1")
            .env("GIT_AUTHOR_NAME", "json-test")
            .env("GIT_AUTHOR_EMAIL", "json@test")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null());
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }

    fn run(&mut self, args: &[&str], keep: bool) -> Output {
        let output = self.command().args(args).output().unwrap();
        self.push(args, &output, keep);
        output
    }

    pub fn push(&mut self, args: &[&str], output: &Output, keep: bool) {
        self.records.push(Record {
            cwd: self.cwd.clone(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            code: output.status.code(),
            streams: keep.then(|| (output.stdout.clone(), output.stderr.clone())),
        });
    }

    /// Runs a command whose output is part of the transcript.
    pub fn json(&mut self, args: &[&str]) -> Output {
        self.run(args, true)
    }

    /// Runs a command that only prepares state: it must succeed, and its
    /// human-readable output is not recorded.
    pub fn setup(&mut self, args: &[&str]) -> Output {
        let output = self.run(args, false);
        assert!(
            output.status.success(),
            "{}: {args:?}: {}",
            self.name,
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    pub fn transcript(&self) -> String {
        let mut raw = Vec::new();
        for record in &self.records {
            let place = if record.cwd.is_empty() {
                String::new()
            } else {
                format!("[{}] ", record.cwd)
            };
            raw.push(format!("$ {place}alt {}\n", record.args.join(" ")));
            raw.push(match record.code {
                Some(code) => format!("exit {code}\n"),
                None => "killed\n".to_owned(),
            });
            if let Some((stdout, stderr)) = &record.streams {
                raw.push(stream("out", stdout));
                raw.push(stream("err", stderr));
            }
        }
        let mut text = raw.concat();
        for (before, after) in &self.between {
            let mut kept = String::new();
            let mut rest = text.as_str();
            while let Some(start) = rest.find(before) {
                let from = start + before.len();
                let length = rest[from..].find(after).unwrap();
                kept.push_str(&rest[..from]);
                kept.push_str("<volatile>");
                rest = &rest[from + length..];
            }
            kept.push_str(rest);
            text = kept;
        }
        normalize(&text, &self.paths, &self.volatile)
    }

    /// With `ALT_JSON_KEEP` set to a directory, the scene's files stay
    /// behind under it for inspection.
    pub fn finish(self) {
        compare(self.name, &self.transcript());
        if let Some(keep) = std::env::var_os("ALT_JSON_KEEP") {
            let kept = Path::new(&keep).join(self.name);
            std::fs::create_dir_all(&keep).unwrap();
            std::fs::rename(self.dir.path(), &kept).unwrap();
            std::fs::write(kept.join("was-at"), self.dir.path().to_str().unwrap()).unwrap();
        }
    }
}

fn stream(label: &str, bytes: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(bytes) else {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        return format!("{label} (not utf-8) {hex}\n");
    };
    let mut shown = String::new();
    for line in text.split_inclusive('\n') {
        shown.push_str(label);
        shown.push_str("| ");
        shown.push_str(line);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        shown.push_str(&format!("\n{label} ends without a newline\n"));
    }
    shown
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cli")
        .join(name)
}

pub fn compare(name: &str, transcript: &str) {
    let path = fixture(&format!("{name}.txt"));
    if std::env::var_os("ALT_JSON_RECORD").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, transcript).unwrap();
    }
    let stored = std::fs::read_to_string(&path).unwrap();
    let differing = stored
        .lines()
        .zip(transcript.lines())
        .position(|(a, b)| a != b);
    if let Some(line) = differing {
        panic!(
            "{name} line {}:\nstored {:?}\nnow    {:?}",
            line + 1,
            stored.lines().nth(line).unwrap(),
            transcript.lines().nth(line).unwrap()
        );
    }
    assert_eq!(stored.len(), transcript.len(), "{name}: length differs");
}

/// A member of the first JSON object a command printed.
pub fn member(output: &Output, key: &str) -> String {
    let value: alt_json::Value = alt_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("no {key} in {value}"))
        .to_owned()
}

fn is_hex(token: &str) -> bool {
    token
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn tokens(text: &str) -> Vec<(bool, &str)> {
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    while start < bytes.len() {
        let word = bytes[start].is_ascii_alphanumeric();
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_alphanumeric() == word {
            end += 1;
        }
        parts.push((word, &text[start..end]));
        start = end;
    }
    parts
}

/// Replaces the scene's directories, the values of volatile members, runs
/// of ten or more digits, and lowercase hexadecimal identifiers of sixteen
/// or more digits. Identifiers are numbered by first appearance, so equal
/// ones stay equal; a shorter hexadecimal word is replaced only when it is
/// the beginning of an identifier that appears in full.
pub fn normalize(text: &str, paths: &[(String, String)], volatile: &[&str]) -> String {
    let mut text = text.to_owned();
    for (path, name) in paths {
        text = text.replace(path, name);
    }
    for key in volatile {
        text = blank_member(&text, key);
    }
    let parts = tokens(&text);
    let digits = |token: &str| token.len() >= 10 && token.bytes().all(|b| b.is_ascii_digit());
    let mut known: Vec<&str> = Vec::new();
    for (word, token) in &parts {
        if *word && token.len() >= 16 && is_hex(token) && !digits(token) && !known.contains(token) {
            known.push(token);
        }
    }
    let mut result = String::new();
    for (word, token) in &parts {
        if !*word || token.len() < 7 || !is_hex(token) {
            result.push_str(token);
        } else if digits(token) {
            result.push_str("<digits>");
        } else if let Some(index) = known.iter().position(|id| id == token) {
            result.push_str(&format!("<id{}:{index}>", token.len()));
        } else if let Some(index) = known.iter().position(|id| id.starts_with(token)) {
            result.push_str(&format!("<short{}:{index}>", token.len()));
        } else {
            result.push_str(token);
        }
    }
    result
}

fn blank_member(text: &str, key: &str) -> String {
    let needle = format!("\"{key}\":");
    let mut result = String::new();
    let mut rest = text;
    while let Some(found) = rest.find(&needle) {
        let after = found + needle.len();
        result.push_str(&rest[..after]);
        rest = &rest[after..];
        let spaces = rest.len() - rest.trim_start_matches(' ').len();
        result.push_str(&rest[..spaces]);
        rest = &rest[spaces..];
        let end = value_end(rest);
        result.push_str("<volatile>");
        rest = &rest[end..];
    }
    result.push_str(rest);
    result
}

/// Where the JSON value at the start of `text` ends.
fn value_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0;
    let mut at = 0;
    loop {
        match bytes[at] {
            b'"' => {
                at += 1;
                while bytes[at] != b'"' {
                    at += if bytes[at] == b'\\' { 2 } else { 1 };
                }
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' if depth > 0 => depth -= 1,
            b',' | b'}' | b']' | b'\n' if depth == 0 => return at,
            _ => {}
        }
        at += 1;
        if depth == 0 && matches!(bytes[at - 1], b'"' | b'}' | b']') {
            return at;
        }
    }
}

#[test]
fn placeholders_keep_identity_and_shape() {
    let paths = [("/tmp/x1".to_owned(), "<root>".to_owned())];
    let text = "{\"oid\":\"0123456789abcdef0123\",\"short\":\"0123456\",\"at\":1731323000123,\
                \"dir\":\"/tmp/x1/repo\",\"again\":\"0123456789abcdef0123\",\"word\":\"defaced\",\
                \"ms\": 12,\"n\":7,\"map\":{\"a\":[1,\"}\"]},\"s\":\"a\\\"b\",\"z\":1}";
    assert_eq!(
        normalize(text, &paths, &["ms", "map", "s"]),
        "{\"oid\":\"<id20:0>\",\"short\":\"<short7:0>\",\"at\":<digits>,\
         \"dir\":\"<root>/repo\",\"again\":\"<id20:0>\",\"word\":\"defaced\",\
         \"ms\": <volatile>,\"n\":7,\"map\":<volatile>,\"s\":<volatile>,\"z\":1}"
    );
}
