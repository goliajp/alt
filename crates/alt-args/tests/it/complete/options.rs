//! Which options are offered, and how they are spelled.

use super::at;

const COMMIT: &str = "-m --message -a --all --everything --amend -n --no-verify --decision \
                      -u --untracked -t -s --workspace -h --help";

#[test]
fn options_are_offered_behind_a_dash_only() {
    assert_eq!(
        at("tool -"),
        ["options: --workspace -v --verbose -h --help -V --version"]
    );
    assert_eq!(
        at("tool --ver"),
        ["options: --workspace --verbose --help --version"]
    );
    let expected = format!("options: {COMMIT}");
    assert_eq!(at("tool commit -"), [expected.as_str(), "value paths [-]"]);
    assert_eq!(at("tool commit "), ["value paths []"]);
    assert_eq!(at("tool commit pa"), ["value paths [pa]"]);
}

#[test]
fn global_options_follow_into_subcommands() {
    assert_eq!(at("tool remote --"), ["options: --json --workspace --help"]);
    assert_eq!(
        at("tool remote add --"),
        ["options: --json --workspace --help"]
    );
    assert_eq!(
        at("tool --workspace w remote add -"),
        ["options: --json --workspace -h --help", "value name [-]"]
    );
    // an option of the top level that is not global stays there
    assert!(!at("tool commit --")[0].contains("--verbose"));
}

#[test]
fn an_option_that_may_not_repeat_is_offered_once() {
    assert!(at("tool commit --")[0].contains("--no-verify"));
    assert!(!at("tool commit -n --")[0].contains("--no-verify"));
    assert!(!at("tool commit --message x --")[0].contains("--message"));
    // an option waiting for its value has appeared as well
    assert_eq!(
        at("tool commit -a -m -")[0],
        "options: -n --no-verify --decision -u --untracked -t -s --workspace -h --help"
    );
    assert!(at("tool commit --decision x --")[0].contains("--decision"));
    assert!(at("tool -v --")[0].contains("--verbose"));
}

#[test]
fn an_option_in_conflict_with_a_given_argument_is_not_offered() {
    assert!(at("tool commit --")[0].contains("--amend"));
    assert!(!at("tool commit --all --")[0].contains("--amend"));
    assert!(!at("tool commit --amend --")[0].contains("--all"));
    assert!(!at("tool commit --amend --")[0].contains("--everything"));
    assert!(at("tool commit --amend --")[0].contains("--no-verify"));
}

#[test]
fn one_option_of_a_group_is_offered_until_one_is_given() {
    assert_eq!(at("tool commit -an"), ["options: -anm -anu -ant -ans -anh"]);
    assert_eq!(at("tool commit -t -an"), ["options: -anm -anu -anh"]);
    assert_eq!(at("tool commit -ant"), ["options: -antm -antu -anth"]);
}

#[test]
fn letters_already_in_a_cluster_have_appeared() {
    assert_eq!(at("tool commit -a"), ["options: -am -an -au -at -as -ah"]);
    assert_eq!(at("tool -v"), ["options: -vv -vh -vV"]);
    assert!(at("tool commit -aZ").is_empty());
    assert!(at("tool commit -Z").is_empty());
}
