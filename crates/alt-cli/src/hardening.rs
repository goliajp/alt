//! Which commands sync what they write before they answer.
//!
//! A command that moves a ref or writes files of the working tree syncs
//! what it wrote, and what it refers to that an earlier command left
//! unsynced, before it answers. `alt add` only stages: by default it
//! leaves what it writes to the kernel, as `git add` does. Where
//! `core.fsync` has `index` among its components (`added` and `all` have
//! it), `alt add` syncs as well.
//!
//! A request whose id the caller chose is synced whatever it does: the
//! caller may send the id again after a power cut, and the receipt must
//! then be there with everything it vouches for. An id drawn for one run
//! is sent again by nobody once that run is over.

use std::path::Path;

use alt_git_config::{Config, IncludeContext};
use alt_os::durability::Unsynced;

use crate::cli::Command;
use crate::native::Identity;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Whether `command` changes the index and the object store and nothing
/// else: no ref, no file of the working tree.
pub fn stages_only(command: &Command) -> bool {
    matches!(command, Command::Add { .. })
}

/// Whether a `core.fsync` of `value` has the index among the components
/// to harden, with the names in it that are not known. The list is read
/// as git reads it: it starts from the default, which does not have the
/// index; `none` empties it, a name adds a component or a group of them,
/// and `-name` takes one out. Only the index is alt's to leave unsynced.
pub fn hardens_index(value: &[u8]) -> (bool, Vec<&[u8]>) {
    const OTHERS: [&[u8]; 8] = [
        b"loose-object",
        b"pack",
        b"pack-metadata",
        b"commit-graph",
        b"objects",
        b"reference",
        b"derived-metadata",
        b"committed",
    ];
    let (mut index, mut unknown) = (false, Vec::new());
    for name in value.split(|byte| *byte == b',') {
        let name = name.trim_ascii_start();
        let (named, on) = match name.strip_prefix(b"-") {
            Some(rest) => (rest, false),
            None => (name, true),
        };
        match named {
            b"index" | b"added" | b"all" => index = on,
            b"none" if on => index = false,
            b"" => {}
            other if OTHERS.contains(&other) => {}
            _ => unknown.push(name),
        }
    }
    (index, unknown)
}

/// Whether the configuration that applies in the repository under
/// `alt_dir`, as the caller `identity` reads it, has `alt add` sync.
fn index_hardened(alt_dir: &Path, identity: &Identity) -> Res<bool> {
    let _caller = identity.outer_environment();
    let dir = alt_dir.join("git-import");
    let mut entries = crate::git_settings::outer(Some(dir.clone()), None)?;
    let file = dir.join("config");
    if file.is_file() {
        let ctx = IncludeContext {
            git_dir: Some(dir),
            branch: None,
            home: crate::git_settings::home(),
        };
        entries.extend(Config::load(&file, &ctx)?.entries);
    }
    let settings = Config { entries };
    let Some(value) = settings.get_str("core", None, "fsync") else {
        return Ok(false);
    };
    let (index, unknown) = hardens_index(value);
    let told = unknown.iter().map(|name| {
        let name = String::from_utf8_lossy(name);
        format!("warning: ignoring unknown core.fsync component '{name}'\n")
    });
    UNKNOWN.set(told.collect());
    Ok(index)
}

thread_local! {
    /// what the command this thread runs is to say of its `core.fsync`
    static UNKNOWN: std::cell::Cell<String> = const { std::cell::Cell::new(String::new()) };
}

/// The warnings of names in `core.fsync` that are not known, for the
/// command the running thread entered ([`enter`]) to give once.
pub fn take_warnings() -> String {
    UNKNOWN.take()
}

/// Has the running thread leave what it writes unsynced where `command`
/// does, until the value returned is dropped. `drawn`: the request id was
/// drawn for this run and no caller chose it.
pub fn enter(command: &Command, drawn: bool, alt_dir: &Path, identity: &Identity) -> Res<Unsynced> {
    // a thread that already leaves its writes unsynced has been through here
    let left = alt_os::durability::unsynced()
        || identity.relaxed()
        || (drawn && stages_only(command) && !index_hardened(alt_dir, identity)?);
    Ok(alt_os::durability::leave_unsynced(left))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_index_is_hardened_where_the_list_ends_up_naming_it() {
        fn index(value: &str) -> (bool, Vec<&[u8]>) {
            hardens_index(value.as_bytes())
        }
        for value in ["index", "added", "all", "committed,index", "none,index"] {
            assert_eq!(index(value), (true, vec![]), "{value}");
        }
        // after a comma git passes spaces over, and nowhere else
        assert_eq!(index("committed, index"), (true, vec![]));
        for value in ["", "committed", "none", "objects,reference", "pack"] {
            assert_eq!(index(value), (false, vec![]), "{value}");
        }
        for value in ["added,-index", "all,-added", "index,none", "all,-all"] {
            assert_eq!(index(value), (false, vec![]), "{value}");
        }
        assert_eq!(index("-index,added"), (true, vec![]));
        // a name git does not know changes nothing, and is told
        assert_eq!(index("ADDED"), (false, vec![&b"ADDED"[..]]));
        assert_eq!(index("added,bogus"), (true, vec![&b"bogus"[..]]));
        assert_eq!(index("added "), (false, vec![&b"added "[..]]));
        assert_eq!(index("-none"), (false, vec![&b"-none"[..]]));
    }
}
