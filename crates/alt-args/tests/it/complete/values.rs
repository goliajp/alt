//! Where the cursor is on a value, and whose.

use alt_args::complete::{Completion, complete};
use alt_args::{Hint, Parser};

use super::{Tool, at};

#[test]
fn the_word_behind_an_option_is_its_value() {
    assert_eq!(at("tool commit -m "), ["value message []"]);
    assert_eq!(at("tool commit --message wi"), ["value message [wi]"]);
    assert_eq!(at("tool --workspace "), ["value workspace []"]);
    assert_eq!(at("tool remote --workspace "), ["value workspace []"]);
    // a lone dash is a value to the parser
    assert!(at("tool commit -m -").contains(&"value message [-]".to_owned()));
}

#[test]
fn a_value_attached_to_its_option_keeps_what_stands_before_it() {
    assert_eq!(
        at("tool commit --message=va"),
        ["value message [va] behind [--message=]"]
    );
    assert_eq!(
        at("tool commit --message="),
        ["value message [] behind [--message=]"]
    );
    assert_eq!(at("tool commit -mva"), ["value message [va] behind [-m]"]);
    assert_eq!(at("tool commit -m=va"), ["value message [va] behind [-m=]"]);
    assert_eq!(at("tool commit -m"), ["value message [] behind [-m]"]);
    assert_eq!(
        at("tool commit -anmva"),
        ["value message [va] behind [-anm]"]
    );
    // a flag takes no value
    assert!(at("tool commit --all=x").is_empty());
    assert!(at("tool commit --nonesuch=x").is_empty());
}

#[test]
fn an_optional_value_is_completed_only_when_attached() {
    assert_eq!(at("tool commit --untracked "), ["value paths []"]);
    assert_eq!(at("tool commit -u n"), ["value paths [n]"]);
    assert_eq!(
        at("tool commit --untracked=n"),
        ["value untracked [n] behind [--untracked=]"]
    );
    assert_eq!(at("tool commit -au"), ["value untracked [] behind [-au]"]);
    assert_eq!(at("tool commit -un"), ["value untracked [n] behind [-u]"]);
}

#[test]
fn positionals_are_filled_in_order() {
    assert_eq!(at("tool switch "), ["value branch []"]);
    assert_eq!(at("tool switch --force ma"), ["value branch [ma]"]);
    assert_eq!(at("tool switch main "), ["value dir []"]);
    assert_eq!(at("tool switch main there "), Vec::<String>::new());
    assert_eq!(at("tool commit a b "), ["value paths []"]);
    assert!(at("tool switch -x").is_empty());
}

#[test]
fn after_a_double_dash_everything_is_a_positional() {
    assert_eq!(at("tool commit -- -"), ["value paths [-]"]);
    assert_eq!(at("tool commit -- --all"), ["value paths [--all]"]);
    assert_eq!(at("tool commit a -- "), ["value paths []"]);
    assert_eq!(at("tool switch -- "), ["value branch []"]);
    assert_eq!(at("tool remote -- "), Vec::<String>::new());
    assert_eq!(at("tool stash -- p"), ["value name [p]"]);
}

#[test]
fn once_a_trailing_positional_starts_every_word_is_its_value() {
    assert_eq!(at("tool run make -"), ["value cmd [-]"]);
    assert_eq!(at("tool run make --quiet"), ["value cmd [--quiet]"]);
    assert_eq!(at("tool run make test "), ["value cmd []"]);
    // before it starts the options of the command are still read
    assert_eq!(
        at("tool run --q"),
        ["options: --quiet --workspace --help", "value cmd [--q]"]
    );
    assert_eq!(at("tool run --quiet "), ["value cmd []"]);
}

#[test]
fn a_positional_that_takes_hyphen_values_is_offered_behind_a_dash() {
    assert_eq!(
        at("tool grep -"),
        [
            "options: -c --count -e --workspace -h --help",
            "value pattern [-]"
        ]
    );
    assert_eq!(
        at("tool grep --co"),
        [
            "options: --count --workspace --help",
            "value pattern [--co]"
        ]
    );
    assert_eq!(at("tool grep -x"), ["value pattern [-x]"]);
    assert_eq!(at("tool grep --zzz=1"), ["value pattern [--zzz=1]"]);
    // a known option stays an option
    assert_eq!(at("tool grep -c"), ["options: -ce -ch"]);
    assert_eq!(at("tool grep -e"), ["value engine [] behind [-e]"]);
    // the parser gives such a word to the option that waits for a value
    assert_eq!(at("tool grep -e -x"), ["value engine [-x]"]);
    // once the positional is filled no other takes the word
    assert!(at("tool grep it -x").is_empty());
}

#[test]
fn a_value_carries_the_hint_and_the_accepted_spellings() {
    let hint = |line: &str| {
        let words: Vec<String> = line.split(' ').map(String::from).collect();
        match complete(Tool::COMMAND, &words, words.len() - 1).as_slice() {
            [Completion::Value { arg, .. }] => (arg.hint, arg.possible),
            _ => panic!("{line}"),
        }
    };
    let none: &[&str] = &[];
    assert_eq!(hint("tool commit "), (Hint::Path, none));
    assert_eq!(hint("tool switch "), (Hint::Other("branch"), none));
    assert_eq!(hint("tool switch main "), (Hint::Dir, none));
    assert_eq!(hint("tool commit -m "), (Hint::None, none));
    assert_eq!(hint("tool --workspace "), (Hint::Other("workspace"), none));
    assert_eq!(hint("tool answer "), (Hint::Unset, none));
    assert_eq!(hint("tool commit -u"), (Hint::Unset, &["no", "all"][..]));
}
