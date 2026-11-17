//! A store a command leaves for the exit of the process to take down is
//! one a drop has nothing to do for: nothing was appended through it,
//! and nothing can be.

use std::ffi::OsString;

use alt_cli::cli::{self, Command};
use alt_cli::native;
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefTarget};

use super::commands::COMMANDS;
use super::states::Base;
use super::{changed, tree};

fn command(args: &[&str]) -> Command {
    let words = std::iter::once("alt").chain(args.iter().copied());
    let parsed = alt_cli::machine::parse(words.map(OsString::from).collect());
    let Ok(parsed) = parsed else {
        panic!("{args:?} does not parse");
    };
    parsed.cli.command
}

#[test]
fn a_store_left_to_the_exit_is_one_that_cannot_append() {
    let base = Base::build();
    let dir = base.state("exit", &[]);
    let text = b"never stored\n";
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, text);
    let (mut left, mut kept) = (0, 0);
    for args in COMMANDS {
        let command = command(args);
        if !cli::is_native(&command) {
            continue;
        }
        let before = tree(&dir);
        let (mut store, _repo) =
            native::open_for_command(&command, dir.join(".alt"), &dir).unwrap();
        if cli::mutates_command(&command) {
            assert!(!store.only_read(), "{args:?}");
            kept += 1;
            continue;
        }
        assert!(store.only_read(), "{args:?}");
        // what a drop would sync or release cannot come to be
        let put = store.odb_mut().put(oid, ObjectKind::Blob, text);
        assert!(put.is_err(), "{args:?}");
        let change = RefChange {
            name: "refs/tags/never".to_owned(),
            old: None,
            new: Some(RefTarget::Oid(oid)),
        };
        let committed = store.refs_mut().commit("test", 0, &[change]);
        assert!(committed.is_err(), "{args:?}");
        assert!(store.only_read(), "{args:?}");
        drop(store);
        let changes = changed(&before, &tree(&dir), &[]);
        assert!(changes.is_empty(), "{args:?} {changes:?}");
        left += 1;
    }
    assert!(left >= 20 && kept >= 20, "{left} left, {kept} kept");
}
