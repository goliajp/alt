//! Compares a variable written into a file's text against what `git
//! config --file` leaves of the same file, byte for byte.

use std::fs;
use std::process::Command;

use alt_git_config::{Name, add, overlay, parse_file, remove_section, rename_section, set, unset};

const FILES: &[&str] = &[
    "",
    "[core]\n\tbare = false\n",
    "[core]\n\tbare = false",
    "# only a comment",
    "[user]\n\tname = A\n\temail = a@b\n[core]\n\tbare = false\n",
    "[user]\n\tname = A # said\n\n; closing\n[core]\n\tbare = false\n",
    "[user] name = A\n[core]\n",
    "[user]\n[core]\n\tx = 1\n[user]\n# last\n",
    "[user]\n    name = \"A \\\n  B\"\n\temail = a@b\n",
    "[User]\n\tName = A\r\n\temail = a@b\r\n",
    "[remote \"origin\"]\n\turl = u\n\tfetch = one\n\tfetch = two\n[user]\n",
    "[remote \"o\\\"r\"]\n\turl = u\n",
    "[user]\n\tname\n\temail = a@b\n",
    "[core]\n\tbare = false\n\n\n",
    "\u{feff}[core]\n\tbare = false\n",
];

const NAMES: &[(&str, Option<&str>, &str)] = &[
    ("user", None, "name"),
    ("user", None, "signingkey"),
    ("core", None, "filemode"),
    ("remote", Some("origin"), "fetch"),
    ("remote", Some("o\"r"), "url"),
    ("branch", Some("with space"), "merge"),
];

const VALUES: &[&str] = &[
    "plain",
    "",
    " led and trailed ",
    "semi;colon # hash",
    "a\tb\nc \"q\" \\ d",
];

fn git(file: &std::path::Path, args: &[&str]) -> Option<Vec<u8>> {
    let run = Command::new("git")
        .args(["config", "--file", file.to_str().unwrap()])
        .args(args)
        .output()
        .unwrap();
    run.status.success().then(|| fs::read(file).unwrap())
}

fn dotted((section, subsection, key): (&str, Option<&str>, &str)) -> String {
    match subsection {
        Some(sub) => format!("{section}.{sub}.{key}"),
        None => format!("{section}.{key}"),
    }
}

#[test]
fn a_variable_is_set_and_added_as_git_writes_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let file = tmp.path().join("config");
    let mut refused = 0;
    for text in FILES {
        for &(section, subsection, key) in NAMES {
            let name = Name {
                section,
                subsection: subsection.map(str::as_bytes),
                key,
            };
            let dotted = dotted((section, subsection, key));
            for value in VALUES {
                let told = format!("{text:?} {dotted} {value:?}");
                fs::write(&file, text).unwrap();
                let theirs = git(&file, &[&dotted, value]);
                let ours = set(text.as_bytes(), name, value.as_bytes());
                match theirs {
                    Some(theirs) => assert_eq!(ours.unwrap(), theirs, "set {told}"),
                    None => {
                        refused += 1;
                        assert!(ours.is_err(), "set {told}")
                    }
                }
                fs::write(&file, text).unwrap();
                let theirs = git(&file, &["--add", &dotted, value]).unwrap();
                let ours = add(text.as_bytes(), name, value.as_bytes()).unwrap();
                assert_eq!(ours, theirs, "add {told}");
            }
        }
    }
    // the one variable a file here sets twice
    assert_eq!(refused, VALUES.len());
}

#[test]
fn an_overlay_is_the_file_git_makes_of_the_same_commands() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let file = tmp.path().join("config");
    let source = "# kept as it is\n[core]\n\tbare = false\n\tfilemode = true ; said\n\
                  [user]\n\tname = Source\n[remote \"origin\"]\n\turl = u\n\tfetch = one\n\
                  \tfetch = two\n[core]\n\tquotepath = off\n";
    // nothing of the store's own, and nothing that reads differently
    assert_eq!(overlay(source.as_bytes(), &[]).unwrap(), source.as_bytes());
    let same = "[remote \"origin\"]\n\tfetch = one\n\tfetch = two\n[user]\n\tname = Source\n";
    assert_eq!(
        overlay(source.as_bytes(), &parse_file(same.as_bytes()).unwrap()).unwrap(),
        source.as_bytes()
    );
    let own = "[user]\n\tname = Own\n\temail = own@example.com\n[core]\n\tfilemode = false\n\
               \tflag\n[remote \"origin\"]\n\tfetch = three\n[alias]\n\tst = status\n\
               [remote \"origin\"]\n\tfetch = four\n\turl = u\n";
    fs::write(&file, source).unwrap();
    for args in [
        &["user.name", "Own"][..],
        &["user.email", "own@example.com"],
        &["core.filemode", "false"],
        &["core.flag", "true"],
        &["--add", "remote.origin.fetch", "three"],
        &["--add", "remote.origin.fetch", "four"],
        &["alias.st", "status"],
    ] {
        git(&file, args).unwrap();
    }
    let ours = overlay(source.as_bytes(), &parse_file(own.as_bytes()).unwrap()).unwrap();
    assert_eq!(
        String::from_utf8(ours.clone()).unwrap(),
        fs::read_to_string(&file).unwrap()
    );
    // what applies is the source's values with the store's own on top
    let read = alt_git_config::Config {
        entries: parse_file(&ours).unwrap(),
    };
    let fetch = read.get_all("remote", Some(b"origin"), "fetch");
    assert_eq!(fetch.len(), 4);
    assert_eq!(
        read.get_str("user", None, "name").unwrap().to_string(),
        "Own"
    );
}

/// Files with comments in and around sections, and sections that come
/// more than once.
const MORE: &[&str] = &[
    "[user]\n\tname = A\n",
    "# about\n[user]\n\tname = A\n[core]\n\tbare = false\n",
    "[core]\n\tbare = false\n[user]\n\tname = A\n# closing\n",
    "[core]\n\tbare = false\n\n[user]\n\tname = A\n\n[alias]\n\tst = status\n",
    "[user]\n\tname = A\n\tname = B\n\temail = e\n",
    "[user]\n\tname = A\n[core]\n\tx = 1\n[user]\n\tname = B\n",
    "[remote \"origin\"]\n\turl = u\n[branch \"with space\"]\n\tmerge = m\n\tremote = r\n",
    "[remote \"origin\"] url = u\n\tfetch = one\n[remote \"o\\\"r\"]\n\turl = v\n",
    "  [remote \"origin\"]\n\tfetch = one ; said\n; between\n[Remote \"origin\"]\n\tfetch = two\n",
];

#[test]
fn a_variable_is_unset_as_git_unsets_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let file = tmp.path().join("config");
    let mut unset_in = 0;
    for text in FILES.iter().chain(MORE) {
        for &(section, subsection, key) in NAMES {
            let name = Name {
                section,
                subsection: subsection.map(str::as_bytes),
                key,
            };
            let dotted = dotted((section, subsection, key));
            fs::write(&file, text).unwrap();
            let theirs = git(&file, &["--unset-all", &dotted]);
            let ours = unset(text.as_bytes(), name).unwrap();
            unset_in += usize::from(ours.is_some());
            let said = |bytes: Option<Vec<u8>>| bytes.map(|b| String::from_utf8(b).unwrap());
            assert_eq!(said(ours), said(theirs), "unset {text:?} {dotted}");
        }
    }
    assert!(unset_in >= 15, "{unset_in}");
}

#[test]
fn a_section_is_removed_and_renamed_as_git_does_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let file = tmp.path().join("config");
    let mut found = 0;
    for text in FILES.iter().chain(MORE) {
        for (section, sub) in [
            ("remote", "origin"),
            ("remote", "o\"r"),
            ("branch", "with space"),
        ] {
            let dotted = format!("{section}.{sub}");
            let said = |bytes: Option<Vec<u8>>| bytes.map(|b| String::from_utf8(b).unwrap());
            fs::write(&file, text).unwrap();
            let theirs = git(&file, &["--remove-section", &dotted]);
            let ours = remove_section(text.as_bytes(), section, sub.as_bytes()).unwrap();
            found += usize::from(ours.is_some());
            assert_eq!(said(ours), said(theirs), "remove {text:?} {dotted}");
            for to in ["other", "a \"q\" \\ b"] {
                fs::write(&file, text).unwrap();
                let renamed = format!("{section}.{to}");
                let theirs = git(&file, &["--rename-section", &dotted, &renamed]);
                let ours = rename_section(text.as_bytes(), section, sub.as_bytes(), to.as_bytes());
                assert_eq!(
                    said(ours.unwrap()),
                    said(theirs),
                    "rename {text:?} {dotted} {to:?}"
                );
            }
        }
    }
    assert!(found > 5, "{found}");
}
