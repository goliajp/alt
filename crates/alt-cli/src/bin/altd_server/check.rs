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

/// The lines of the users file `text` to look at, each with why; and the
/// lines the server does not read, with which it does not start.
pub(super) fn looked_at(text: &str) -> (Vec<String>, Vec<String>) {
    let (mut told, mut unread) = (Vec::new(), Vec::new());
    if text.starts_with('\u{feff}') {
        let mark = "begins with a byte order mark, which the server takes for the name";
        told.push(format!("line 1: {mark}"));
    }
    for (at, line) in text.lines().enumerate() {
        let at = at + 1;
        let Some((user, _, rules)) = fields(line) else {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                let passed = "no tab after the name: the server passes the line over";
                told.push(format!("line {at}: {passed}"));
            }
            continue;
        };
        let Some(rules) = rules else {
            let all = "no rules: every repository is read and written";
            told.push(format!("line {at}: {user}: {all}"));
            continue;
        };
        let acl = match parse_acl(rules) {
            Ok(acl) => acl,
            Err(why) => {
                unread.push(format!("line {at}: {why}"));
                continue;
            }
        };
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
    (told, unread)
}

/// Prints what is to be looked at in the file at `path` and ends the process:
/// with 0 where there is nothing, 1 where there is, 2 where it does not read.
/// Every line the server does not read is told, not the first alone.
pub(super) fn run(path: &str) -> ! {
    let text = std::fs::read_to_string(path);
    let text = text.unwrap_or_else(|error| super::die(&format!("{path}: {error}")));
    let (told, unread) = looked_at(&text);
    told.iter().for_each(|line| println!("{line}"));
    unread.iter().for_each(|line| eprintln!("{path}: {line}"));
    std::process::exit(match (unread.is_empty(), told.is_empty()) {
        (false, _) => 2,
        (true, false) => 1,
        (true, true) => 0,
    });
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
            // blanks where the tabs belong: no hash, and the server passes it over
            "kate 11 *:rw",
        ]
        .join("\n");
        let (told, unread) = looked_at(&file);
        assert!(unread.is_empty());
        let expected = [
            "line 2: alice: no rules: every repository is read and written",
            "line 3: bob: docs was read and written and is written",
            "line 4: carol: secret was read and written and is closed",
            "line 7: frank: no rules: every repository is read and written",
            "line 8: grace: docs was read and written and is written",
            "line 9: heidi: docs was read and written and is read",
            "line 10: ivan: docs was read and written and is written",
            "line 12: no tab after the name: the server passes the line over",
        ];
        assert_eq!(told, expected);
        // every line that does not read is told, and the others beside them
        let (told, unread) = looked_at("\u{feff}al\t1\nbob\t11\tdocs:x *:rw\ncy\t2\t:rw\n");
        assert_eq!(told.len(), 2, "{told:?}");
        assert!(told[0].starts_with("line 1: begins with a byte order mark"));
        assert_eq!(unread.len(), 2, "{unread:?}");
        assert!(unread[0].starts_with("line 2: \"docs:x\" is no rule"));
        assert!(unread[1].starts_with("line 3: "));
        assert_eq!(looked_at(""), (Vec::new(), Vec::new()));
    }
}
