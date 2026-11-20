//! The static description of a command line.

use crate::value::Check;

/// One command: the program itself or a subcommand.
pub struct Command {
    pub name: &'static str,
    /// one line of help, empty for none
    pub about: &'static str,
    /// printed by `--version`; empty where the flag does not exist
    pub version: &'static str,
    /// whether `help [COMMAND]...` is a subcommand
    pub help_subcommand: bool,
    pub items: &'static [Item],
    pub subcommands: &'static [Command],
}

pub enum Item {
    Arg(Arg),
    /// arguments recorded together under a name, or under none when it is
    /// empty; a struct or variant makes one
    Group(&'static str, &'static [Item]),
    /// where the subcommands sit among the arguments, for help ordering
    Subcommands,
}

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

pub enum Preset {
    None,
    Text(&'static str),
    Computed(fn() -> String),
}

/// One argument. Every argument of every command sits in the data the
/// loader relocates before `main`, a page of which costs each start a copy:
/// what few arguments set is kept apart in [`Rare`].
pub struct Arg {
    pub id: &'static str,
    /// empty for none; an argument with neither `long` nor `short` is positional
    pub long: &'static str,
    pub short: Option<char>,
    pub value_name: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub required: bool,
    /// also accepted by every subcommand below
    pub global: bool,
    /// the only accepted values when not empty
    pub possible: &'static [&'static str],
    pub check: Option<Check>,
    /// once this positional starts, every later word belongs to it
    pub trailing: bool,
    /// words starting with `-` are values of this positional
    pub hyphen_values: bool,
    /// `None` where none of it is set
    pub rare: Option<&'static Rare>,
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
    pub conflicts: &'static [&'static str],
    pub requires: &'static [&'static str],
    /// required unless one of these is present
    pub required_unless: &'static [&'static str],
    /// at most one argument of a named group may be present
    pub group: &'static str,
}

impl Rare {
    pub const NONE: Rare = Rare {
        alias: "",
        preset: Preset::None,
        missing: "",
        conflicts: &[],
        requires: &[],
        required_unless: &[],
        group: "",
    };
}

pub(crate) static HELP: Arg = Arg {
    id: "help",
    long: "help",
    short: Some('h'),
    help: "Print help",
    kind: Kind::Help,
    ..Arg::NEW
};

pub(crate) static VERSION: Arg = Arg {
    id: "version",
    long: "version",
    short: Some('V'),
    help: "Print version",
    kind: Kind::Version,
    ..Arg::NEW
};

impl Arg {
    pub const NEW: Arg = Arg {
        id: "",
        long: "",
        short: None,
        value_name: "",
        help: "",
        kind: Kind::Flag,
        required: false,
        global: false,
        possible: &[],
        check: None,
        trailing: false,
        hyphen_values: false,
        rare: None,
    };

    pub(crate) fn rare(&self) -> &'static Rare {
        self.rare.unwrap_or(&Rare::NONE)
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
        if !self.rare().missing.is_empty() {
            return text + &format!("[=<{}>]", self.value_name);
        }
        if !self.is_positional() {
            text.push(' ');
        }
        if self.is_positional() && !required.unwrap_or(self.required) {
            text += &format!("[{}]", self.value_name);
        } else {
            text += &format!("<{}>", self.value_name);
        }
        if self.is_positional() && self.kind == Kind::Multiple {
            text += "...";
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn an_argument_takes_no_more_room_than_what_every_argument_sets() {
        // four names, the accepted values, the check, the rare settings,
        // the short name and five one-byte fields, rounded up to eight
        assert_eq!(
            size_of::<Arg>(),
            (4 * 16 + 16 + 8 + 8 + 4 + 5_usize).next_multiple_of(8)
        );
        assert!(size_of::<Item>() <= size_of::<Arg>() + 8);
    }
}
