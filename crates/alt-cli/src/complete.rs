//! `alt complete`: what a shell may offer at the cursor of an `alt`
//! command line.
//!
//! The answer is lines of fields separated by tabs:
//!
//! ```text
//! group<TAB>name<TAB>title    the candidates that follow belong to this group
//! value<TAB>description       one candidate
//! files<TAB>file|dir          the shell completes file names itself
//! nospace                     no space follows a candidate named since the last group line
//! prefix<TAB>text             what follows stands behind this text in the word
//! ```
//!
//! In a field a tab is written `\t`, a line break `\n` and a backslash
//! `\\`; a candidate spelled like the first field of another kind of line
//! has a backslash behind its first letter.

use alt_args::complete::{Candidate, Completion, complete};
use alt_args::{Hint, Parser};

use crate::daemon::Response;

#[derive(alt_args::Args, Clone)]
pub struct CompleteArgs {
    /// The shell that asks
    #[arg(long, value_parser = ["zsh"], hint = none)]
    pub shell: String,
    /// Which of the words the cursor is in, the program being word 0
    #[arg(long, hint = none)]
    pub current: usize,
    /// The words of the command line, the program first
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hint = none)]
    pub words: Vec<String>,
}

/// The groups candidates are offered in, each with the title a shell
/// shows above it.
pub mod group {
    pub type Group = (&'static str, &'static str);
    pub const COMMANDS: Group = ("commands", "command");
    pub const OPTIONS: Group = ("options", "option");
    pub const VALUES: Group = ("values", "value");
    pub const BRANCHES: Group = ("branches", "branch");
    pub const TAGS: Group = ("tags", "tag");
    pub const REMOTE_BRANCHES: Group = ("remote-branches", "remote branch");
    pub const COMMITS: Group = ("commits", "commit");
    pub const REFS: Group = ("refs", "ref");
    pub const REMOTES: Group = ("remotes", "remote");
    pub const WORKSPACES: Group = ("workspaces", "workspace");
    pub const MODIFIED_FILES: Group = ("modified-files", "modified file");
    pub const UNTRACKED_FILES: Group = ("untracked-files", "untracked file");
    pub const UNMERGED_FILES: Group = ("unmerged-files", "unmerged file");
    pub const TRACKED_FILES: Group = ("tracked-files", "tracked file");
    pub const CONFIG_KEYS: Group = ("config-keys", "configuration key");
}

const DIRECTIVES: [&str; 4] = ["group", "files", "nospace", "prefix"];

fn field(out: &mut Vec<u8>, text: &str) {
    for byte in text.bytes() {
        match byte {
            b'\t' => out.extend(b"\\t"),
            b'\n' => out.extend(b"\\n"),
            b'\\' => out.extend(b"\\\\"),
            _ => out.push(byte),
        }
    }
}

/// The lines of an answer.
#[derive(Default)]
pub struct Answer {
    pub text: Vec<u8>,
}

impl Answer {
    fn line(&mut self, fields: &[&str]) {
        for (at, text) in fields.iter().enumerate() {
            if at > 0 {
                self.text.push(b'\t');
            }
            field(&mut self.text, text);
        }
        self.text.push(b'\n');
    }

    pub fn group(&mut self, (name, title): group::Group) {
        self.line(&["group", name, title]);
    }

    pub fn candidate(&mut self, value: &str, description: &str) {
        if DIRECTIVES.contains(&value) {
            self.text.push(value.as_bytes()[0]);
            self.text.push(b'\\');
            field(&mut self.text, &value[1..]);
            self.text.push(b'\t');
            field(&mut self.text, description);
            self.text.push(b'\n');
            return;
        }
        self.line(&[value, description]);
    }

    /// A group of candidates that have no description; nothing when there
    /// are none.
    pub fn listed<T: AsRef<str>>(&mut self, group: group::Group, values: &[T]) {
        if values.is_empty() {
            return;
        }
        self.group(group);
        for value in values {
            self.candidate(value.as_ref(), "");
        }
    }

    pub fn files(&mut self, directories: bool) {
        self.line(&["files", if directories { "dir" } else { "file" }]);
    }

    pub fn nospace(&mut self) {
        self.line(&["nospace"]);
    }

    pub fn prefix(&mut self, text: &str) {
        self.line(&["prefix", text]);
    }
}

/// A value whose candidates only the repository knows.
pub struct Asked<'a> {
    /// the kind the argument declares
    pub kind: &'static str,
    /// what is typed of the value so far
    pub typed: &'a str,
    /// what stands before the value in its word
    pub attached: &'a str,
    /// the words of the line and which of them the cursor is in
    pub words: &'a [String],
    pub current: usize,
}

impl Asked<'_> {
    /// The words of the line the cursor is not in, the program left out.
    pub fn others(&self) -> impl Iterator<Item = &str> {
        let words = self.words.iter().enumerate().skip(1);
        words
            .filter(|(at, _)| *at != self.current)
            .map(|(_, word)| word.as_str())
    }
}

fn described(answer: &mut Answer, group: group::Group, candidates: &[Candidate]) {
    answer.group(group);
    for candidate in candidates {
        answer.candidate(&candidate.value, candidate.help);
    }
}

/// Writes the answer for `args`. What the command line itself says is
/// always in it; `repository` adds the candidates of a value only the
/// repository knows.
pub fn answer(args: &CompleteArgs, repository: &mut dyn FnMut(&Asked, &mut Answer)) -> Answer {
    let mut answer = Answer::default();
    let command = crate::cli::Cli::command();
    for completion in complete(command, &args.words, args.current) {
        match completion {
            Completion::Subcommands(candidates) => {
                described(&mut answer, group::COMMANDS, &candidates)
            }
            Completion::Options(candidates) => described(&mut answer, group::OPTIONS, &candidates),
            Completion::Value {
                arg,
                prefix,
                attached,
            } => {
                let attached = attached.unwrap_or_default();
                if !attached.is_empty() {
                    answer.prefix(&attached);
                }
                answer.listed(group::VALUES, &arg.possible().to_vec());
                match arg.hint() {
                    Hint::Unset | Hint::None => {}
                    Hint::Path => answer.files(false),
                    Hint::Dir => answer.files(true),
                    Hint::Other(kind) => {
                        let asked = Asked {
                            kind,
                            typed: &prefix,
                            attached: &attached,
                            words: &args.words,
                            current: args.current,
                        };
                        repository(&asked, &mut answer);
                    }
                }
            }
        }
    }
    answer
}

/// Answers a shell. `in_repository` runs the command as any other is
/// run; its answer is given when it succeeds, else what the command line
/// itself says. Never an error and nothing on stderr, which a shell would
/// print into the line being typed.
pub fn respond(args: &CompleteArgs, in_repository: impl FnOnce() -> Response) -> Response {
    let has_repository = || {
        let cwd = std::env::current_dir();
        cwd.is_ok_and(|cwd| crate::native::resolve_workspace(&cwd, None).is_ok())
    };
    respond_where(args, has_repository, in_repository)
}

fn respond_where(
    args: &CompleteArgs,
    has_repository: impl FnOnce() -> bool,
    in_repository: impl FnOnce() -> Response,
) -> Response {
    let mut asks = false;
    let syntax = answer(args, &mut |_, _| asks = true).text;
    if asks && has_repository() {
        let response = in_repository();
        if response.exit_code == 0 {
            return Response {
                stderr: Vec::new(),
                ..response
            };
        }
    }
    Response {
        exit_code: 0,
        stdout: syntax,
        stderr: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_escaped_and_no_candidate_reads_as_another_kind_of_line() {
        let mut answer = Answer::default();
        answer.group(group::BRANCHES);
        answer.candidate("a\tb", "one\ntwo \\ three");
        answer.candidate("files", "dir");
        answer.candidate("nospace", "");
        answer.candidate("group", "");
        answer.candidate("prefix", "x");
        answer.candidate("groups", "");
        answer.nospace();
        assert_eq!(
            String::from_utf8(answer.text).unwrap(),
            "group\tbranches\tbranch\na\\tb\tone\\ntwo \\\\ three\nf\\iles\tdir\nn\\ospace\t\n\
             g\\roup\t\np\\refix\tx\ngroups\t\nnospace\n"
        );
    }

    #[test]
    fn an_empty_group_is_not_announced() {
        let mut answer = Answer::default();
        answer.listed::<&str>(group::TAGS, &[]);
        assert!(answer.text.is_empty());
    }

    fn asked(line: &str) -> CompleteArgs {
        let words: Vec<String> = line.split(' ').map(String::from).collect();
        CompleteArgs {
            shell: "zsh".to_owned(),
            current: words.len() - 1,
            words,
        }
    }

    fn said(exit_code: u8, stdout: &str, stderr: &str) -> Response {
        Response {
            exit_code,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn shown(response: Response) -> (u8, String, String) {
        let text = |bytes: Vec<u8>| String::from_utf8(bytes).unwrap();
        (
            response.exit_code,
            text(response.stdout),
            text(response.stderr),
        )
    }

    #[test]
    fn the_repository_is_asked_only_for_what_it_alone_knows() {
        let unasked = || -> Response { panic!("asked the repository") };
        // a line that asks for subcommands needs no repository
        let commands = respond_where(&asked("alt "), || true, unasked);
        assert!(shown(commands).1.starts_with("group\tcommands\tcommand\n"));
        // and there is none to ask outside one
        let outside = respond_where(&asked("alt switch "), || false, unasked);
        assert_eq!(shown(outside), (0, String::new(), String::new()));
    }

    #[test]
    fn what_the_repository_answers_is_given_without_its_stderr() {
        let warned = || said(0, "group\tbranches\tbranch\nmain\t\n", "warning: x\n");
        let answered = respond_where(&asked("alt switch "), || true, warned);
        let expected = "group\tbranches\tbranch\nmain\t\n".to_owned();
        assert_eq!(shown(answered), (0, expected, String::new()));
    }

    #[test]
    fn a_repository_that_fails_leaves_what_the_command_line_says() {
        let failed = || said(128, "half an answer", "fatal: x\n");
        let values = respond_where(&asked("alt init "), || true, || -> Response { panic!() });
        assert_eq!(shown(values), (0, "files\tdir\n".to_owned(), String::new()));
        let nothing = respond_where(&asked("alt switch "), || true, failed);
        assert_eq!(shown(nothing), (0, String::new(), String::new()));
    }
}
