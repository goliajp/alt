//! The editor side of an interactive rebase: the todo list format, running
//! the user's editor on it and on commit messages, and git's message cleanup.

use super::*;

/// One todo-list command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Pick,
    Reword,
    Squash,
    Fixup,
    Drop,
}

impl Action {
    pub(super) fn word(self) -> &'static str {
        match self {
            Action::Pick => "pick",
            Action::Reword => "reword",
            Action::Squash => "squash",
            Action::Fixup => "fixup",
            Action::Drop => "drop",
        }
    }

    fn parse(word: &str) -> Option<Action> {
        Some(match word {
            "pick" | "p" => Action::Pick,
            "reword" | "r" => Action::Reword,
            "squash" | "s" => Action::Squash,
            "fixup" | "f" => Action::Fixup,
            "drop" | "d" => Action::Drop,
            _ => return None,
        })
    }

    /// Folds into the previous commit rather than making its own.
    pub(super) fn melds(self) -> bool {
        matches!(self, Action::Squash | Action::Fixup)
    }
}

/// One line of a todo list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) enum Step {
    /// a commit to replay
    Replay(Action, ObjectId),
    /// a shell command to run
    Run(String),
    /// a name for where the rebase stands
    Label(String),
    /// stand where a label, or else a revision, names
    Reset(String),
    /// make this merge commit again, of where the rebase stands with what
    /// each word names as a label or else as a revision
    Merge(ObjectId, Vec<String>),
}

pub(super) const TODO_HELP: &str = "\
#
# Commands:
# p, pick <commit> = use commit
# r, reword <commit> = use commit, but edit the commit message
# s, squash <commit> = use commit, but meld into previous commit
# f, fixup <commit> = like \"squash\" but keep only the previous commit's message
# d, drop <commit> = remove commit
# x, exec <command> = run command (the rest of the line) using shell
# l, label <label> = label current HEAD with a name
# t, reset <label> = reset HEAD to a label
# m, merge -C <commit> <label> = make the merge commit again, with its
#                                 message, of HEAD and the label
#
# These lines can be re-ordered; they are executed from top to bottom.
#
# If you remove a line here THAT COMMIT WILL BE LOST.
#
# However, if you remove everything, the rebase will be aborted.
#
";

/// Parses an edited todo list; `resolve` turns the abbreviated oid into a
/// commit. Blank lines and `#` comments are skipped; what follows the
/// words a line needs is only a note.
pub(super) fn parse_todo(text: &str, resolve: impl Fn(&str) -> Res<ObjectId>) -> Res<Vec<Step>> {
    let mut steps = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let missing = |what: &str| format!("rebase: missing {what} on todo line {}", n + 1);
        let mut words = rest
            .split('#')
            .next()
            .unwrap_or_default()
            .split_whitespace();
        let mut next = |what: &str| words.next().ok_or_else(|| missing(what));
        steps.push(match word {
            "exec" | "x" => Step::Run(rest.trim().to_owned()),
            "label" | "l" => Step::Label(next("label")?.to_owned()),
            "reset" | "t" => Step::Reset(next("label")?.to_owned()),
            "merge" | "m" => {
                if next("-C")? != "-C" {
                    let said = "rebase: a merge in the todo list takes its message with -C";
                    return Err(format!("{said} (line {})", n + 1).into());
                }
                let commit = resolve(next("commit")?)?;
                let first = next("label")?.to_owned();
                let more = words.map(str::to_owned);
                Step::Merge(commit, std::iter::once(first).chain(more).collect())
            }
            _ => {
                let unknown = || format!("rebase: unknown command '{word}' on todo line {}", n + 1);
                let action = Action::parse(word).ok_or_else(unknown)?;
                Step::Replay(action, resolve(next("commit")?)?)
            }
        });
    }
    let made = |step: &&Step| matches!(step, Step::Replay(..) | Step::Merge(..));
    if let Some(Step::Replay(first, _)) = steps.iter().find(made)
        && first.melds()
    {
        return Err(format!(
            "rebase: cannot '{}' without a previous commit",
            first.word()
        )
        .into());
    }
    Ok(steps)
}

/// Runs the user's editor on `path`, looked up as git does: for the todo
/// list `GIT_SEQUENCE_EDITOR` first, then `GIT_EDITOR`, `VISUAL`, `EDITOR`,
/// and `vi`. The value is a shell command, so arguments in it work.
pub(super) fn edit(path: &Path, todo_list: bool) -> Res<()> {
    check_machine_mode()?;
    let set = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let editor = todo_list
        .then(|| set("GIT_SEQUENCE_EDITOR"))
        .flatten()
        .or_else(|| set("GIT_EDITOR"))
        .or_else(|| set("VISUAL"))
        .or_else(|| set("EDITOR"))
        .unwrap_or_else(|| "vi".to_owned());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor)
        .arg(path)
        .status()
        .map_err(|e| format!("could not run the editor '{editor}': {e}"))?;
    if !status.success() {
        return Err(format!("the editor '{editor}' exited with {status}").into());
    }
    Ok(())
}

/// git's default message cleanup after an editor: drop `#` lines and
/// trailing whitespace, fold runs of blank lines, trim blank lines at both
/// ends, end with a newline.
pub(super) fn cleanup(message: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in message.lines() {
        if line.starts_with('#') {
            continue;
        }
        let line = line.trim_end();
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line);
    }
    while out.last() == Some(&"") {
        out.pop();
    }
    if out.is_empty() {
        return String::new();
    }
    out.join("\n") + "\n"
}

pub(super) fn check_machine_mode() -> Res<()> {
    if crate::diagnostics::json_mode() {
        return Err(crate::machine::Failure::refusal(
            "interactive_input_required",
            "interactive editors are unavailable in JSON mode; supply an explicit todo or message",
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_matches_git_strip() {
        assert_eq!(cleanup("# c\n\n\nA  \n\n\n\nB\n# c\n\n"), "A\n\nB\n");
        assert_eq!(cleanup("# only comments\n"), "");
    }

    #[test]
    fn todo_lines_parse_with_abbreviations_and_comments() {
        let oid =
            |s: &str| -> Res<ObjectId> { Ok(format!("{s:0<40}").parse::<ObjectId>().unwrap()) };
        let steps = parse_todo("# x\npick a1 one\n\nf b2 two\nd c3\n", oid).unwrap();
        let actions = steps.iter().map(|s| match s {
            Step::Replay(action, _) => *action,
            other => panic!("{other:?}"),
        });
        let actions: Vec<Action> = actions.collect();
        assert_eq!(actions, [Action::Pick, Action::Fixup, Action::Drop]);
        assert!(parse_todo("squash a1 x\n", oid).is_err());
        assert!(parse_todo("edit a1 x\n", oid).is_err());
    }

    #[test]
    fn the_lines_of_a_rebase_that_keeps_merges_are_read() {
        let oid =
            |s: &str| -> Res<ObjectId> { Ok(format!("{s:0<40}").parse::<ObjectId>().unwrap()) };
        let text = "label onto\nreset onto\npick a1 # one\nx make test\nl side\n\
                    t 0b # base\nmerge -C c3 side # Merge branch 'side'\n";
        let steps = parse_todo(text, oid).unwrap();
        let (a1, c3) = (oid("a1").unwrap(), oid("c3").unwrap());
        let expected = [
            Step::Label("onto".into()),
            Step::Reset("onto".into()),
            Step::Replay(Action::Pick, a1),
            Step::Run("make test".into()),
            Step::Label("side".into()),
            Step::Reset("0b".into()),
            Step::Merge(c3, vec!["side".into()]),
        ];
        assert_eq!(steps, expected);
        assert!(parse_todo("merge side\n", oid).is_err());
        assert!(parse_todo("label\n", oid).is_err());
    }
}
