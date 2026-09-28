//! Commit metadata kept in git notes under [`NOTES_REF`]: structured fields
//! about each commit (what kind of change, who made it and for whom, which
//! paths it touched, the decisions an agent recorded) and an optional
//! replacement message. Commit objects stay plain git, so their ids never
//! change and a git remote only ever sees the commits themselves.
//!
//! A note reads like mail headers, then an optional message after a blank
//! line, so `git notes --ref=alt/meta show <commit>` is readable as is:
//!
//! ```text
//! schema: alt-meta/1
//! type: feat
//! summary: handle Result types
//! author-type: agent
//! touched: src/parse.rs
//! decision: chose Result over Option to match the parser
//! ```

use std::sync::LazyLock;

use alt_git_codec::{EntryMode, HashAlgo, ObjectId, ObjectKind, Tree, TreeEntry};
use bstr::BString;
use regex::Regex;

pub const NOTES_REF: &str = "refs/notes/alt/meta";
pub const SCHEMA: &str = "alt-meta/1";

/// At most this many recorded decisions per commit, one line each.
pub const MAX_DECISIONS: usize = 3;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// One commit's note: ordered header fields and an optional message that
/// stands in for the commit's own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub fields: Vec<(String, String)>,
    pub message: Option<String>,
}

impl Meta {
    pub fn parse(text: &str) -> Meta {
        let (head, body) = match text.split_once("\n\n") {
            Some((h, b)) => (h, Some(b)),
            None => (text.trim_end_matches('\n'), None),
        };
        let fields = head
            .lines()
            .filter_map(|l| l.split_once(": "))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let message = body.filter(|b| !b.trim().is_empty()).map(str::to_owned);
        Meta { fields, message }
    }

    pub fn render(&self) -> String {
        let mut out: String = self
            .fields
            .iter()
            .map(|(k, v)| format!("{k}: {v}\n"))
            .collect();
        if let Some(m) = &self.message {
            out.push('\n');
            out.push_str(m);
            if !m.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.fields
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn push(&mut self, key: &str, value: impl Into<String>) {
        self.fields.push((key.to_owned(), value.into()));
    }

    /// Replaces every `key` field with one holding `value`.
    pub fn set(&mut self, key: &str, value: &str) {
        self.fields.retain(|(k, _)| k != key);
        self.push(key, value);
    }
}

/// What a commit is recorded with besides its message.
pub struct Facts<'a> {
    pub author_type: &'a str,
    pub author: &'a str,
    pub controlling: Option<&'a str>,
    pub session: Option<&'a str>,
    pub touched: Vec<String>,
}

/// Derives a note for a commit with `message`, then adds what the caller
/// chose to record: `decisions` (one line each, at most [`MAX_DECISIONS`])
/// and `extra` fields (`key=value`, stored as `x-key`).
pub fn derive(
    message: &str,
    facts: Facts<'_>,
    decisions: &[String],
    extra: &[(String, String)],
) -> Res<Meta> {
    static CONVENTIONAL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([a-z]+)(\([^)]*\))?(!)?: (.+)$").expect("built-in pattern")
    });
    let recorded = recorded(decisions, extra)?;
    let subject = message.lines().next().unwrap_or("");
    let mut m = Meta::default();
    m.push("schema", SCHEMA);
    match CONVENTIONAL.captures(subject) {
        Some(c) => {
            m.push("type", &c[1]);
            m.push("summary", &c[4]);
            let breaking = c.get(3).is_some() || message.contains("\nBREAKING CHANGE: ");
            if breaking {
                m.push("breaking", "true");
            }
        }
        None => m.push("summary", subject),
    }
    m.push("author-type", facts.author_type);
    m.push("author", facts.author);
    if let Some(c) = facts.controlling {
        m.push("controlling", c);
    }
    if let Some(s) = facts.session {
        m.push("session", s);
    }
    for t in facts.touched {
        m.push("touched", t);
    }
    m.fields.extend(recorded);
    Ok(m)
}

/// Checks what a caller chose to record and turns it into note fields:
/// `decision` lines (one line each, at most [`MAX_DECISIONS`]) and `x-key`
/// fields.
pub fn recorded(decisions: &[String], extra: &[(String, String)]) -> Res<Vec<(String, String)>> {
    if decisions.len() > MAX_DECISIONS {
        return Err(format!("at most {MAX_DECISIONS} decisions per commit").into());
    }
    let mut out = Vec::new();
    for d in decisions {
        if d.contains('\n') || d.trim().is_empty() {
            return Err(format!("a decision is one non-empty line: {d:?}").into());
        }
        out.push(("decision".to_owned(), d.trim().to_owned()));
    }
    for (k, v) in extra {
        let valid = !k.is_empty()
            && k.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !valid || v.contains('\n') {
            return Err(
                format!("bad metadata field {k}={v:?}: lowercase key, one-line value").into(),
            );
        }
        out.push((format!("x-{k}"), v.clone()));
    }
    Ok(out)
}

/// The fields a rewritten commit (amend, rebase) keeps from its original
/// note: what someone chose to record, not what was derived.
pub fn carry_over(from: &Meta, onto: &mut Meta) {
    for (k, v) in &from.fields {
        if k == "decision" || k.starts_with("x-") {
            onto.fields.push((k.clone(), v.clone()));
        }
    }
    if onto.message.is_none() {
        onto.message = from.message.clone();
    }
}

/// What reading an object gives: its kind and bytes, or `None` when absent.
pub type ReadResult = Res<Option<(ObjectKind, Vec<u8>)>>;
/// What storing an object gives: its id.
pub type WriteResult = Res<ObjectId>;
/// Reads an object.
pub type Read<'a> = dyn Fn(ObjectId) -> ReadResult + 'a;
/// Stores an object.
pub type Write<'a> = dyn FnMut(ObjectKind, &[u8]) -> WriteResult + 'a;

/// The note for `target` in the notes commit `notes`, flat or fanned out.
pub fn find(read: &Read, algo: HashAlgo, notes: ObjectId, target: ObjectId) -> Res<Option<Meta>> {
    let root = commit_tree(read, notes)?;
    let hex = target.to_string();
    let tree = tree_at(read, algo, root)?;
    let blob = match tree.entries.iter().find(|e| e.name == hex.as_bytes()) {
        Some(e) => Some(e.oid),
        None => match tree.entries.iter().find(|e| e.name == hex.as_bytes()[..2]) {
            Some(dir) => tree_at(read, algo, dir.oid)?
                .entries
                .into_iter()
                .find(|e| e.name == hex.as_bytes()[2..])
                .map(|e| e.oid),
            None => None,
        },
    };
    match blob {
        Some(b) => {
            let (_, data) = read(b)?.ok_or("note blob missing from store")?;
            Ok(Some(Meta::parse(&String::from_utf8_lossy(&data))))
        }
        None => Ok(None),
    }
}

/// Writes `meta` for `target` on top of the notes commit `parent` (none for
/// the first note) and returns the new notes commit. Notes go under a
/// two-character fan-out directory, so one update rewrites one small tree.
pub fn store(
    read: &Read,
    write: &mut Write,
    algo: HashAlgo,
    parent: Option<ObjectId>,
    target: ObjectId,
    meta: &Meta,
    signature: &str,
) -> Res<ObjectId> {
    let hex = target.to_string();
    let (fan, rest) = hex.split_at(2);
    let mut root = match parent {
        Some(p) => tree_at(read, algo, commit_tree(read, p)?)?,
        None => Tree {
            entries: Vec::new(),
        },
    };
    // a flat entry for this commit (written by git itself) gives way
    root.entries.retain(|e| e.name != hex.as_bytes());
    let mut sub = match root.entries.iter().find(|e| e.name == fan.as_bytes()) {
        Some(e) => tree_at(read, algo, e.oid)?,
        None => Tree {
            entries: Vec::new(),
        },
    };
    let blob = write(ObjectKind::Blob, meta.render().as_bytes())?;
    put_entry(&mut sub, rest, EntryMode::from_bytes(b"100644")?, blob);
    let sub_id = write(ObjectKind::Tree, &sub.serialize())?;
    put_entry(&mut root, fan, EntryMode::from_bytes(b"40000")?, sub_id);
    let root_id = write(ObjectKind::Tree, &root.serialize())?;

    let mut body = format!("tree {root_id}\n");
    if let Some(p) = parent {
        body.push_str(&format!("parent {p}\n"));
    }
    body.push_str(&format!(
        "author {signature}\ncommitter {signature}\n\nNotes added by 'alt'\n"
    ));
    write(ObjectKind::Commit, body.as_bytes())
}

fn commit_tree(read: &Read, commit: ObjectId) -> Res<ObjectId> {
    let (_, data) = read(commit)?.ok_or("notes commit missing from store")?;
    Ok(alt_git_codec::Commit::parse(&data)?
        .tree()
        .ok_or("notes commit without a tree")?)
}

fn tree_at(read: &Read, algo: HashAlgo, id: ObjectId) -> Res<Tree> {
    let (_, data) = read(id)?.ok_or("notes tree missing from store")?;
    Ok(Tree::parse(&data, algo)?)
}

/// Inserts or replaces `name`, keeping git's tree order (a directory sorts
/// as if its name ended in `/`).
fn put_entry(tree: &mut Tree, name: &str, mode: EntryMode, oid: ObjectId) {
    tree.entries.retain(|e| e.name != name.as_bytes());
    tree.entries.push(TreeEntry {
        mode,
        name: BString::from(name),
        oid,
    });
    let key = |e: &TreeEntry| {
        let mut k = e.name.to_vec();
        if e.mode.as_bytes() == b"40000" {
            k.push(b'/');
        }
        k
    };
    tree.entries.sort_by_key(key);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(touched: &[&str]) -> Facts<'static> {
        Facts {
            author_type: "agent",
            author: "bot-a",
            controlling: Some("owner@example.org"),
            session: None,
            touched: touched.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn conventional_subjects_become_type_and_summary() {
        let m = derive(
            "feat(parser)!: handle Result types\n",
            facts(&["src/p.rs"]),
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(m.get("type"), Some("feat"));
        assert_eq!(m.get("summary"), Some("handle Result types"));
        assert_eq!(m.get("breaking"), Some("true"));
        assert_eq!(m.all("touched").collect::<Vec<_>>(), ["src/p.rs"]);
        let plain = derive("Fix the thing\n", facts(&[]), &[], &[]).unwrap();
        assert_eq!(plain.get("type"), None);
        assert_eq!(plain.get("summary"), Some("Fix the thing"));
    }

    #[test]
    fn decisions_and_extra_fields_are_bounded() {
        let d = |n: usize| (0..n).map(|i| format!("d{i}")).collect::<Vec<_>>();
        assert!(derive("x\n", facts(&[]), &d(3), &[]).is_ok());
        assert!(derive("x\n", facts(&[]), &d(4), &[]).is_err());
        assert!(derive("x\n", facts(&[]), &["two\nlines".into()], &[]).is_err());
        let extra = [("model".to_string(), "m-1".to_string())];
        assert_eq!(
            derive("x\n", facts(&[]), &[], &extra)
                .unwrap()
                .get("x-model"),
            Some("m-1")
        );
        assert!(derive("x\n", facts(&[]), &[], &[("Bad Key".into(), "v".into())]).is_err());
    }

    #[test]
    fn a_note_survives_render_and_parse_with_its_message() {
        let mut m = derive(
            "feat: a\n",
            facts(&["a", "b"]),
            &["kept it small".into()],
            &[],
        )
        .unwrap();
        m.message = Some("feat: a, described properly\n\nwith a body\n".into());
        assert_eq!(Meta::parse(&m.render()), m);
    }

    #[test]
    fn rewrites_carry_recorded_fields_but_not_derived_ones() {
        let mut old = derive(
            "x\n",
            facts(&["old"]),
            &["because"].map(String::from),
            &[("k".into(), "v".into())],
        )
        .unwrap();
        old.message = Some("better\n".into());
        let mut new = derive("x\n", facts(&["new"]), &[], &[]).unwrap();
        carry_over(&old, &mut new);
        assert_eq!(new.all("touched").collect::<Vec<_>>(), ["new"]);
        assert_eq!(new.get("decision"), Some("because"));
        assert_eq!(new.get("x-k"), Some("v"));
        assert_eq!(new.message.as_deref(), Some("better\n"));
    }
}
