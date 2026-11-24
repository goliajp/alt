//! `altd-server check-users <file>`: what of a users file an operator is to
//! look at, read as the server reads it.
//!
//! The rules of a user were once added up: `*:r docs:w` let `docs` be read
//! and written. Now the rules that name a repository decide for it, and `*`
//! for the others: `docs` is written and not read. A file written for the
//! first reading gives less under the second, and which lines those are is
//! told here, with the lines that give everything: a user with no column of
//! rules is held to none.

use super::auth::{AclRule, Action, acl_allows, fields, parse_acl};

/// What the rules allowed for `repo` when they were added up.
fn added_up(acl: &[AclRule], repo: &str, action: Action) -> bool {
    let named = acl
        .iter()
        .filter(|rule| rule.repo == repo || rule.repo == "*");
    named.into_iter().any(|rule| match action {
        Action::Read => rule.can_read,
        Action::Write => rule.can_write,
    })
}

fn said(read: bool, write: bool) -> &'static str {
    match (read, write) {
        (true, true) => "read and written",
        (true, false) => "read",
        (false, true) => "written",
        (false, false) => "closed",
    }
}

/// The lines of the users file `text` to look at, each with why; an error
/// for a line the server does not read.
pub(super) fn looked_at(text: &str) -> Result<Vec<String>, String> {
    let mut told = Vec::new();
    for (at, line) in text.lines().enumerate() {
        let Some((user, _, rules)) = fields(line) else {
            continue;
        };
        let at = at + 1;
        let Some(rules) = rules else {
            let all = "no rules: every repository is read and written";
            told.push(format!("line {at}: {user}: {all}"));
            continue;
        };
        let acl = parse_acl(rules).map_err(|why| format!("line {at}: {why}"))?;
        let mut seen = Vec::new();
        for rule in acl.iter().filter(|rule| rule.repo != "*") {
            if seen.contains(&&rule.repo) {
                continue;
            }
            seen.push(&rule.repo);
            let may = |of: fn(&[AclRule], &str, Action) -> bool| {
                (
                    of(&acl, &rule.repo, Action::Read),
                    of(&acl, &rule.repo, Action::Write),
                )
            };
            let (before, now) = (may(added_up), may(acl_allows));
            if before != now {
                told.push(format!(
                    "line {at}: {user}: {} was {} and is {}",
                    rule.repo,
                    said(before.0, before.1),
                    said(now.0, now.1)
                ));
            }
        }
    }
    Ok(told)
}

/// Prints what is to be looked at in the file at `path` and ends the process:
/// with 0 where there is nothing, 1 where there is, 2 where it does not read.
pub(super) fn run(path: &str) -> ! {
    let text = std::fs::read_to_string(path);
    let told = text
        .map_err(|error| error.to_string())
        .and_then(|text| looked_at(&text));
    match told {
        Err(why) => super::die(&format!("{path}: {why}")),
        Ok(told) => {
            told.iter().for_each(|line| println!("{line}"));
            std::process::exit(i32::from(!told.is_empty()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lines_that_give_less_than_they_did_and_those_that_give_all_are_told() {
        let file = [
            "# users",
            "alice\t00",
            "bob\t11\t*:r docs:w",
            "carol\t22\tsecret:n *:rw",
            "dave\t33\t*:rw",
            "erin\t44\tdocs:rw",
            // the third column is there and blank: no column, as the server reads it
            "frank\t55\t",
            // rules a tab apart, a line that begins with one, and other blanks between rules
            "grace\t66\t*:r\tdocs:w",
            "\theidi\t77\t*:rw docs:r",
            "ivan\t88\t*:r\u{a0}docs:w\u{3000}more:rw",
            "judy\t99\tdocs:r docs:w *:n",
        ]
        .join("\n");
        let told = looked_at(&file).unwrap();
        let expected = [
            "line 2: alice: no rules: every repository is read and written",
            "line 3: bob: docs was read and written and is written",
            "line 4: carol: secret was read and written and is closed",
            "line 7: frank: no rules: every repository is read and written",
            "line 8: grace: docs was read and written and is written",
            "line 9: heidi: docs was read and written and is read",
            "line 10: ivan: docs was read and written and is written",
        ];
        assert_eq!(told, expected);
        let why = looked_at("bob\t11\tdocs:x *:rw").unwrap_err();
        assert!(why.starts_with("line 1: \"docs:x\" is no rule"), "{why}");
        assert!(looked_at("").unwrap().is_empty());
    }
}
