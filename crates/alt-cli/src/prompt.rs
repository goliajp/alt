//! `alt prompt`: the state of the repository a shell prompt shows, from
//! one command.
//!
//! The default output is a line for each thing there is to say, a key and
//! its value; the keys and their order are fixed, and what does not apply
//! is left out:
//!
//! ```text
//! repo /path/to/root
//! workspace default
//! branch develop          or `detached 6d1f3128`, or `unborn main`
//! tag v0.1.0              HEAD is the commit of this tag
//! operation rebase        merge, rebase, cherry-pick, revert or bisect
//! step 3 7                which of how many commits the operation is at
//! staged 2
//! unstaged 1
//! untracked 4
//! unmerged 1
//! remote origin 2 0       ahead of and behind the branch of that name there
//! attention unconfirmed   unconfirmed, recovery or damaged
//! ```

use crate::daemon::Response;

#[derive(alt_args::Args, Clone)]
pub struct PromptArgs {
    /// `lines` for a key and its value on each line, `zsh` for one line a
    /// zsh evaluates into the associative array `alt_prompt`
    #[arg(long, value_parser = ["lines", "zsh"], default_value = "lines", hint = none)]
    pub format: String,
    /// Do not look for untracked files
    #[arg(long)]
    pub no_untracked: bool,
    /// Do not count commits against the remotes
    #[arg(long)]
    pub no_ahead_behind: bool,
}

/// What there is to say, in the order it is said.
#[derive(Default)]
pub struct State {
    pub lines: Vec<(&'static str, String)>,
}

fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

impl State {
    pub fn say(&mut self, key: &'static str, value: impl Into<String>) {
        self.lines.push((key, value.into()));
    }

    /// The state where nothing but the place of the repository can be
    /// read.
    pub fn unreadable(root: &std::path::Path) -> State {
        let mut state = State::default();
        state.say("repo", root.display().to_string());
        state.say("attention", "damaged");
        state
    }

    fn as_lines(&self) -> String {
        let lines = self.lines.iter();
        lines
            .map(|(key, value)| format!("{key} {value}\n"))
            .collect()
    }

    /// `typeset -gA alt_prompt=(key value ...)`: each line under its key,
    /// a remote under `remote:<name>`, and the names of the remotes under
    /// `remotes`.
    fn as_zsh(&self) -> String {
        let mut text = "typeset -gA alt_prompt=(".to_owned();
        let mut remotes = Vec::new();
        for (key, value) in &self.lines {
            let (key, value) = match (*key, value.split_once(' ')) {
                ("remote", Some((name, counts))) => {
                    remotes.push(name);
                    (format!("remote:{name}"), counts)
                }
                _ => ((*key).to_owned(), value.as_str()),
            };
            text += &format!("{} {} ", quoted(&key), quoted(value));
        }
        if !remotes.is_empty() {
            text += &format!("remotes {} ", quoted(&remotes.join(" ")));
        }
        text.trim_end().to_owned() + ")\n"
    }

    pub fn render(&self, format: &str) -> String {
        match format {
            "zsh" => self.as_zsh(),
            _ => self.as_lines(),
        }
    }
}

/// Answers a prompt. Outside a repository there is nothing to say and the
/// exit code is 1. In one, `in_repository` runs the command as any other
/// is run; where that fails the prompt is told the repository needs
/// attention, and never anything on stderr, which would be printed above
/// every prompt.
pub fn respond(args: &PromptArgs, in_repository: impl FnOnce() -> Response) -> Response {
    let cwd = std::env::current_dir();
    let found = cwd.map(|cwd| crate::native::resolve_workspace(&cwd, None));
    let root = match &found {
        Ok(Ok((_, coord))) => Some(coord.root()),
        _ => None,
    };
    respond_at(args, root, in_repository)
}

/// [`respond`] where `root` is the working tree of the repository the
/// command is run in, if it is run in one.
fn respond_at(
    args: &PromptArgs,
    root: Option<&std::path::Path>,
    in_repository: impl FnOnce() -> Response,
) -> Response {
    let Some(root) = root else {
        return Response {
            exit_code: 1,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
    };
    let response = in_repository();
    let stdout = match response.exit_code {
        0 => response.stdout,
        _ => State::unreadable(root).render(&args.format).into_bytes(),
    };
    Response {
        exit_code: 0,
        stdout,
        stderr: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        let mut state = State::default();
        state.say("repo", "/work/it's here");
        state.say("branch", "develop");
        state.say("step", "3 7");
        state.say("remote", "origin 2 0");
        state.say("remote", "backup 10000+ 4+");
        state
    }

    #[test]
    fn lines_are_a_key_and_its_value() {
        assert_eq!(
            state().render("lines"),
            "repo /work/it's here\nbranch develop\nstep 3 7\nremote origin 2 0\n\
             remote backup 10000+ 4+\n"
        );
    }

    #[test]
    fn zsh_gets_one_assignment_with_every_value_quoted() {
        assert_eq!(
            state().render("zsh"),
            "typeset -gA alt_prompt=('repo' '/work/it'\\''s here' 'branch' 'develop' \
             'step' '3 7' 'remote:origin' '2 0' 'remote:backup' '10000+ 4+' \
             remotes 'origin backup')\n"
        );
        assert_eq!(
            State::default().render("zsh"),
            "typeset -gA alt_prompt=()\n"
        );
    }

    fn asked(format: &str) -> PromptArgs {
        PromptArgs {
            format: format.to_owned(),
            no_untracked: false,
            no_ahead_behind: false,
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
    fn outside_a_repository_the_command_is_not_run() {
        let unasked = || -> Response { panic!("ran the command") };
        let outside = respond_at(&asked("lines"), None, unasked);
        assert_eq!(shown(outside), (1, String::new(), String::new()));
    }

    #[test]
    fn what_the_command_says_is_given_without_its_stderr() {
        let root = std::path::Path::new("/work/r");
        let warned = || said(0, "repo /work/r\nbranch main\n", "warning: x\n");
        let told = respond_at(&asked("lines"), Some(root), warned);
        let expected = "repo /work/r\nbranch main\n".to_owned();
        assert_eq!(shown(told), (0, expected, String::new()));
    }

    #[test]
    fn a_command_that_fails_is_told_as_a_repository_that_needs_attention() {
        let root = std::path::Path::new("/work/r");
        let failed = || said(128, "half", "fatal: x\n");
        let told = respond_at(&asked("lines"), Some(root), failed);
        let expected = "repo /work/r\nattention damaged\n".to_owned();
        assert_eq!(shown(told), (0, expected, String::new()));
        let told = respond_at(&asked("zsh"), Some(root), failed);
        let expected = "typeset -gA alt_prompt=('repo' '/work/r' 'attention' 'damaged')\n";
        assert_eq!(shown(told), (0, expected.to_owned(), String::new()));
    }
}
