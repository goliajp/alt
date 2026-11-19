//! One command level with the arguments it accepts.

use crate::spec::{Arg, Command, HELP, Item, VERSION};

pub(crate) struct Slot {
    pub(crate) arg: &'static Arg,
    /// position among the options and subcommands of the declaring command
    pub(crate) order: usize,
    /// the enclosing group at the declaring command; empty once inherited
    pub(crate) owner: &'static str,
}

pub(crate) struct Scope {
    pub(crate) command: &'static Command,
    /// the program name and the subcommand names down to this command
    pub(crate) path: String,
    /// own arguments in declaration order, the inherited global ones, then
    /// the built-in help and version
    pub(crate) slots: Vec<Slot>,
}

/// One letter of a `-abc` word.
pub(crate) enum Letter<'a> {
    Unknown(char),
    /// a flag, by slot
    Flag(usize),
    /// an option that takes a value, by slot, and the value attached to it
    Valued(usize, Option<&'a str>),
}

/// The name and the attached value of `--name` or `--name=value`.
pub(crate) fn split_long(word: &str) -> (&str, Option<&str>) {
    match word[2..].split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (&word[2..], None),
    }
}

fn collect(
    items: &'static [Item],
    owner: &'static str,
    subcommands: usize,
    order: &mut usize,
    slots: &mut Vec<Slot>,
) {
    for item in items {
        match item {
            Item::Arg(arg) => {
                slots.push(Slot {
                    arg,
                    order: *order,
                    owner,
                });
                if !arg.is_positional() {
                    *order += 1;
                }
            }
            Item::Group(name, items) => collect(items, name, subcommands, order, slots),
            Item::Subcommands => *order += subcommands,
        }
    }
}

impl Scope {
    pub(crate) fn root(command: &'static Command, program: String) -> Scope {
        Scope::new(command, program, &[])
    }

    pub(crate) fn child(&self, command: &'static Command) -> Scope {
        Scope::new(
            command,
            format!("{} {}", self.path, command.name),
            &self.slots,
        )
    }

    fn new(command: &'static Command, path: String, inherited: &[Slot]) -> Scope {
        let mut slots = Vec::new();
        collect(
            command.items,
            "",
            command.subcommands.len(),
            &mut 0,
            &mut slots,
        );
        for slot in inherited.iter().filter(|slot| slot.arg.global) {
            slots.push(Slot {
                arg: slot.arg,
                order: slot.order,
                owner: "",
            });
        }
        let builtin = [&HELP, &VERSION].into_iter();
        for arg in builtin.take(if command.version.is_empty() { 1 } else { 2 }) {
            slots.push(Slot {
                arg,
                order: usize::MAX,
                owner: "",
            });
        }
        Scope {
            command,
            path,
            slots,
        }
    }

    pub(crate) fn find(&self, id: &str) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.arg.id == id)
    }

    pub(crate) fn long(&self, name: &str) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| !name.is_empty() && (slot.arg.long == name || slot.arg.alias == name))
    }

    pub(crate) fn short(&self, name: char) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.arg.short == Some(name))
    }

    /// The letters of `-abc`, each with its offset behind the `-`, up to
    /// the first that is unknown or takes a value.
    pub(crate) fn cluster<'a>(&self, word: &'a str) -> Vec<(usize, Letter<'a>)> {
        let cluster = &word[1..];
        let mut letters = Vec::new();
        for (offset, letter) in cluster.char_indices() {
            let Some(at) = self.short(letter) else {
                letters.push((offset, Letter::Unknown(letter)));
                break;
            };
            if !self.slots[at].arg.takes_value() {
                letters.push((offset, Letter::Flag(at)));
                continue;
            }
            let attached = &cluster[offset + letter.len_utf8()..];
            let value = attached.strip_prefix('=').unwrap_or(attached);
            letters.push((
                offset,
                Letter::Valued(at, (!attached.is_empty()).then_some(value)),
            ));
            break;
        }
        letters
    }

    pub(crate) fn positionals(&self) -> impl Iterator<Item = (usize, &Slot)> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.arg.is_positional())
    }

    pub(crate) fn has_positionals(&self) -> bool {
        self.positionals().next().is_some()
    }

    /// Every long name accepted here.
    pub(crate) fn longs(&self) -> Vec<&'static str> {
        let mut longs = Vec::new();
        for slot in &self.slots {
            longs.extend(
                [slot.arg.long, slot.arg.alias]
                    .into_iter()
                    .filter(|name| !name.is_empty()),
            );
        }
        longs
    }

    pub(crate) fn subcommand_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.command.subcommands.iter().map(|c| c.name).collect();
        if self.command.help_subcommand {
            names.push("help");
        }
        names
    }

    /// The subcommands that are shown to the user.
    pub(crate) fn listed_subcommands(&self) -> impl Iterator<Item = &'static Command> {
        self.command.subcommands.iter().filter(|c| !c.hidden)
    }

    pub(crate) fn listed_subcommand_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.listed_subcommands().map(|c| c.name).collect();
        if self.command.help_subcommand {
            names.push("help");
        }
        names
    }
}
