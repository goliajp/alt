//! What a command line said, before it is read into typed fields.

use crate::spec::Command;

/// The values of one argument or the members of one group.
#[derive(Clone)]
pub struct Matched {
    pub id: &'static str,
    /// a flag holds `true` or `false`; a group holds the ids of its members
    /// that appeared, one per appearance
    pub values: Vec<String>,
    /// given on the command line rather than filled in from a default
    pub(crate) explicit: bool,
}

/// One command level; the chosen subcommand is the next level.
pub struct Matches {
    pub(crate) command: &'static Command,
    pub(crate) entries: Vec<Matched>,
    pub(crate) subcommand: Option<Box<Matches>>,
}

const CHECKED: &str = "values are checked while parsing";

impl Matches {
    pub fn name(&self) -> &'static str {
        self.command.name
    }

    pub fn entries(&self) -> &[Matched] {
        &self.entries
    }

    pub fn subcommand(&self) -> Option<&Matches> {
        self.subcommand.as_deref()
    }

    /// The subcommand level of a command that requires one.
    pub fn sub(&self) -> &Matches {
        self.subcommand().expect("a subcommand is required")
    }

    pub(crate) fn entry(&self, id: &str) -> Option<&Matched> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub(crate) fn upsert(&mut self, matched: Matched) {
        match self.entries.iter_mut().find(|entry| entry.id == matched.id) {
            Some(entry) => *entry = matched,
            None => self.entries.push(matched),
        }
    }

    pub fn flag(&self, id: &str) -> bool {
        self.one(id) == Some("true")
    }

    pub fn one(&self, id: &str) -> Option<&str> {
        self.all(id).first().map(String::as_str)
    }

    pub fn all(&self, id: &str) -> &[String] {
        self.entry(id).map_or(&[], |entry| &entry.values)
    }

    pub fn required<T, E>(&self, id: &str, parse: impl Fn(&str) -> Result<T, E>) -> T {
        self.optional(id, parse)
            .expect("a required argument is present")
    }

    pub fn optional<T, E>(&self, id: &str, parse: impl Fn(&str) -> Result<T, E>) -> Option<T> {
        self.one(id).map(|text| parse(text).ok().expect(CHECKED))
    }

    pub fn repeated<T, E>(&self, id: &str, parse: impl Fn(&str) -> Result<T, E>) -> Vec<T> {
        self.all(id)
            .iter()
            .map(|text| parse(text).ok().expect(CHECKED))
            .collect()
    }
}
