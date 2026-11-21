//! One argument of a command.

use crate::settings::{Computed, Hint, Kind, Preset, Rare, Words};
use crate::spec::Node;
use crate::text::Reader;

const REQUIRED: usize = 1;
const GLOBAL: usize = 2;
const TRAILING: usize = 4;
const HYPHEN_VALUES: usize = 8;
const CHECKED: usize = 16;
const TYPED: usize = 32;
const LISTED: usize = 64;

const ALIAS: usize = 1;
const PRESET: usize = 2;
const MISSING: usize = 4;
const CONFLICTS: usize = 8;
const REQUIRES: usize = 16;
const REQUIRED_UNLESS: usize = 32;
const GROUP: usize = 64;

/// One argument, read out of the description of its command.
///
/// ```text
/// argument = kind:1 flags:1 rare:1 number:2 short:short id:short long:short rest:long
/// rest     = [ alias:short preset:1 [ text:long ] missing:long conflicts:long
///              requires:long required_unless:long group:short ]
///            value_name:short help:long listed:long hint:1 [ kind:short ]
/// ```
///
/// `hint` is 0 for none declared, 1 for no candidates, 2 for a path, 3
/// for a directory and 4 for a kind of the program's own, whose name
/// follows.
///
/// `rare` has a bit for each of the settings in brackets that is set, and
/// the brackets are there when it is not 0. What every command line is
/// read by comes first and is read at once; the rest is read when it is
/// asked for.
#[derive(Clone, Copy)]
pub struct Arg {
    pub id: &'static str,
    /// empty for none; an argument with neither `long` nor `short` is positional
    pub long: &'static str,
    pub short: Option<char>,
    pub kind: Kind,
    pub required: bool,
    /// also accepted by every subcommand below
    pub global: bool,
    /// once this positional starts, every later word belongs to it
    pub trailing: bool,
    /// words starting with `-` are values of this positional
    pub hyphen_values: bool,
    /// the type of the argument checks its values
    checked: bool,
    /// the type of the argument says which values it accepts
    typed: bool,
    /// the description lists the accepted values
    listed: bool,
    /// which of the settings few arguments have are set
    rare: u8,
    /// which of the arguments of its type this is
    number: u16,
    /// the type whose description this is read from; none for a built-in
    node: Option<Node>,
    rest: &'static str,
}

pub(crate) const HELP: Arg = Arg {
    id: "help",
    long: "help",
    short: Some('h'),
    // value name, help, listed values, hint
    rest: "\0\0\u{a}Print help\0\0\0",
    kind: Kind::Help,
    ..Arg::NEW
};

pub(crate) const VERSION: Arg = Arg {
    id: "version",
    long: "version",
    short: Some('V'),
    rest: "\0\0\u{d}Print version\0\0\0",
    kind: Kind::Version,
    ..Arg::NEW
};

impl Arg {
    const NEW: Arg = Arg {
        id: "",
        long: "",
        short: None,
        kind: Kind::Flag,
        required: false,
        global: false,
        trailing: false,
        hyphen_values: false,
        checked: false,
        typed: false,
        listed: false,
        rare: 0,
        number: 0,
        node: None,
        rest: "",
    };

    /// Reads the argument `reader` stands at and leaves it behind it.
    pub(crate) fn read(node: Node, reader: &mut Reader) -> Arg {
        let kind = Kind::numbered(reader.one());
        let flags = reader.one();
        let rare = reader.one() as u8;
        let number = reader.two() as u16;
        Arg {
            short: reader.short().chars().next(),
            id: reader.short(),
            long: reader.short(),
            kind,
            required: flags & REQUIRED != 0,
            global: flags & GLOBAL != 0,
            trailing: flags & TRAILING != 0,
            hyphen_values: flags & HYPHEN_VALUES != 0,
            checked: flags & CHECKED != 0,
            typed: flags & TYPED != 0,
            listed: flags & LISTED != 0,
            rare,
            number,
            node: Some(node),
            rest: reader.long(),
        }
    }

    /// The settings few arguments have, when this one has any.
    fn settings(&self) -> Option<(Node, Reader)> {
        let node = self.node.filter(|_| self.rare != 0)?;
        Some((node, Reader::new(self.rest, 0)))
    }

    /// The text of one of the settings few arguments have: the first
    /// `skipped` of them are passed over.
    fn setting(&self, which: usize, skipped: usize) -> &'static str {
        let Some((_, mut reader)) = self.settings().filter(|_| self.has(which)) else {
            return "";
        };
        let alias = reader.short();
        if which == ALIAS {
            return alias;
        }
        if reader.one() == 1 {
            reader.long();
        }
        for _ in 1..skipped {
            reader.long();
        }
        match which {
            GROUP => reader.short(),
            _ => reader.long(),
        }
    }

    /// What few arguments set.
    pub fn rare(&self) -> Rare {
        let Some((node, mut reader)) = self.settings() else {
            return Rare::NONE;
        };
        let alias = reader.short();
        let preset = match reader.one() {
            0 => Preset::None,
            1 => Preset::Text(reader.long()),
            _ => Preset::Computed(Computed::new(node, usize::from(self.number))),
        };
        Rare {
            alias,
            preset,
            missing: reader.long(),
            conflicts: Words(reader.long()),
            requires: Words(reader.long()),
            required_unless: Words(reader.long()),
            group: reader.short(),
        }
    }

    /// The value name, the help and the listed values begin here.
    fn common(&self) -> Reader {
        let mut reader = Reader::new(self.rest, 0);
        if self.rare != 0 {
            reader.short();
            if reader.one() == 1 {
                reader.long();
            }
            for _ in 0..4 {
                reader.long();
            }
            reader.short();
        }
        reader
    }

    pub fn value_name(&self) -> &'static str {
        self.common().short()
    }

    pub fn help(&self) -> &'static str {
        let mut reader = self.common();
        reader.short();
        reader.long()
    }

    /// The accepted values the description lists.
    fn listed(&self) -> Words {
        if !self.listed {
            return Words::NONE;
        }
        let mut reader = self.common();
        reader.short();
        reader.long();
        Words(reader.long())
    }

    /// What kind of thing a value of this argument is.
    pub fn hint(&self) -> Hint {
        let mut reader = self.common();
        reader.short();
        reader.long();
        reader.long();
        match reader.one() {
            0 => Hint::Unset,
            1 => Hint::None,
            2 => Hint::Path,
            3 => Hint::Dir,
            _ => Hint::Other(reader.short()),
        }
    }

    fn has(&self, setting: usize) -> bool {
        usize::from(self.rare) & setting != 0
    }

    /// A second long name shown in help, empty for none.
    pub fn alias(&self) -> &'static str {
        self.setting(ALIAS, 0)
    }

    pub fn preset(&self) -> Preset {
        match self.has(PRESET) {
            true => self.rare().preset,
            false => Preset::None,
        }
    }

    /// The value an option has when it is given without one, empty for
    /// an option that must have one.
    pub fn missing(&self) -> &'static str {
        self.setting(MISSING, 1)
    }

    pub fn conflicts(&self) -> Words {
        Words(self.setting(CONFLICTS, 2))
    }

    pub fn requires(&self) -> Words {
        Words(self.setting(REQUIRES, 3))
    }

    /// Required unless one of these is present.
    pub fn required_unless(&self) -> Words {
        Words(self.setting(REQUIRED_UNLESS, 4))
    }

    /// At most one argument of a named group may be present.
    pub fn group(&self) -> &'static str {
        self.setting(GROUP, 5)
    }

    /// The only accepted values when not empty.
    pub fn possible(&self) -> Words {
        match self.node {
            Some(node) if self.typed => node.typed(usize::from(self.number)),
            _ => self.listed(),
        }
    }

    /// Whether `value` is one this argument takes, with the reason when
    /// its type has one.
    pub(crate) fn check(&self, value: &str) -> Result<(), String> {
        match self.node {
            Some(node) if self.checked => node.check(usize::from(self.number), value),
            _ if self.listed && !self.listed().contains(value) => Err(String::new()),
            _ => Ok(()),
        }
    }

    pub(crate) fn is_positional(&self) -> bool {
        self.long.is_empty() && self.short.is_none()
    }

    pub(crate) fn takes_value(&self) -> bool {
        matches!(self.kind, Kind::Single | Kind::Multiple)
    }

    /// Whether this is the built-in `--help` or `--version`.
    pub(crate) fn is_builtin(&self) -> bool {
        matches!(self.kind, Kind::Help | Kind::Version)
    }

    /// `--long <NAME>`, `-s`, `<NAME>` or `[NAME]...`; `required` picks the
    /// brackets of a positional and defaults to the argument's own setting.
    pub(crate) fn display(&self, required: Option<bool>) -> String {
        let mut text = if !self.long.is_empty() {
            format!("--{}", self.long)
        } else if let Some(short) = self.short {
            format!("-{short}")
        } else {
            String::new()
        };
        if !self.takes_value() {
            return text;
        }
        if !self.missing().is_empty() {
            return text + &format!("[=<{}>]", self.value_name());
        }
        if !self.is_positional() {
            text.push(' ');
        }
        if self.is_positional() && !required.unwrap_or(self.required) {
            text += &format!("[{}]", self.value_name());
        } else {
            text += &format!("<{}>", self.value_name());
        }
        if self.is_positional() && self.kind == Kind::Multiple {
            text += "...";
        }
        text
    }
}
