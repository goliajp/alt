//! What an argument is set to be.

use crate::spec::Node;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// present or absent
    Flag,
    /// a flag that may be given several times; how often is its value
    Count,
    /// a flag that may be given several times, each time bare or with a
    /// value written against its letter (`-C`, `-C40`)
    Attached,
    /// one value
    Single,
    /// any number of values
    Multiple,
    /// prints the help text and stops
    Help,
    /// prints the version line and stops
    Version,
}

impl Kind {
    pub(crate) fn numbered(number: usize) -> Kind {
        match number {
            0 => Kind::Flag,
            1 => Kind::Count,
            2 => Kind::Attached,
            3 => Kind::Single,
            _ => Kind::Multiple,
        }
    }
}

/// Words kept as one text, a line each.
#[derive(Clone, Copy)]
pub struct Words(pub &'static str);

impl Words {
    pub const NONE: Words = Words("");

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The words, for as long as the caller needs them.
    pub fn iter<'a>(self) -> std::str::Lines<'a> {
        self.0.lines()
    }

    pub fn contains(&self, word: &str) -> bool {
        self.iter().any(|own| own == word)
    }

    pub fn to_vec(&self) -> Vec<&'static str> {
        self.iter().collect()
    }
}

impl IntoIterator for Words {
    type Item = &'static str;
    type IntoIter = std::str::Lines<'static>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// A default that is computed when it is needed.
#[derive(Clone, Copy)]
pub struct Computed {
    node: Node,
    number: usize,
}

impl Computed {
    pub(crate) fn new(node: Node, number: usize) -> Computed {
        Computed { node, number }
    }

    pub fn value(&self) -> String {
        self.node.preset(self.number)
    }
}

#[derive(Clone, Copy)]
pub enum Preset {
    None,
    Text(&'static str),
    Computed(Computed),
}

/// The settings of an argument that few arguments have.
pub struct Rare {
    /// a second long name shown in help, empty for none
    pub alias: &'static str,
    pub preset: Preset,
    /// the value an option has when it is given without one; not empty
    /// makes the value optional, and one is then only taken when attached
    /// (`--name=value`, `-nvalue`), never from the next word
    pub missing: &'static str,
    pub conflicts: Words,
    pub requires: Words,
    /// required unless one of these is present
    pub required_unless: Words,
    /// at most one argument of a named group may be present
    pub group: &'static str,
}

impl Rare {
    pub(crate) const NONE: Rare = Rare {
        alias: "",
        preset: Preset::None,
        missing: "",
        conflicts: Words::NONE,
        requires: Words::NONE,
        required_unless: Words::NONE,
        group: "",
    };
}
