//! The shell commands of `alt rebase --exec`. The rebase stops before
//! each and holds nothing while it runs, so that the command may itself
//! be an `alt` that writes to the same workspace; the process that was
//! given the rebase runs the command and then goes on with the rebase.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::daemon::Response;

/// A command a rebase stopped for, and the directory it is to run in.
struct Asked {
    command: String,
    root: PathBuf,
}

static ASKED: Mutex<Option<Asked>> = Mutex::new(None);
static JUST_RAN: Mutex<Option<String>> = Mutex::new(None);

/// A rebase asks for `command` to be run in `root` once it has ended.
pub(crate) fn ask(command: String, root: PathBuf) {
    *ASKED.lock().expect("not poisoned") = Some(Asked { command, root });
}

/// The command that ended well just before this rebase went on.
pub(crate) fn just_ran() -> Option<String> {
    JUST_RAN.lock().expect("not poisoned").clone()
}

/// Writes out what a rebase said before it stopped for a command; with
/// `json` it said nothing of its own on standard output yet.
fn written(response: &Response, json: bool) -> std::io::Result<()> {
    if !json {
        let mut out = std::io::stdout().lock();
        out.write_all(&response.stdout)?;
        out.flush()?;
    }
    std::io::stderr().lock().write_all(&response.stderr)
}

/// After `response`, that of a rebase: runs each command the rebase
/// stopped for and has the rebase go `on`, until it stops for none.
/// What a rebase said before a command is written out before it runs.
/// With `json` the command's standard output goes to standard error,
/// and standard output holds the result of the rebase alone.
pub fn run(mut response: Response, json: bool, on: &mut dyn FnMut() -> Response) -> Response {
    loop {
        let asked = ASKED.lock().expect("not poisoned").take();
        let Some(Asked { command, root }) = asked else {
            return response;
        };
        if written(&response, json).is_err() {
            response.exit_code = 75;
            (response.stdout, response.stderr) = Default::default();
            return response;
        }
        eprintln!("Executing: {command}");
        let mut shell = Command::new("sh");
        shell.arg("-c").arg(&command).current_dir(&root);
        if json {
            shell.stdout(Stdio::from(std::io::stderr()));
        }
        let ended = shell.status();
        if !ended.as_ref().is_ok_and(|status| status.success()) {
            let said = format!(
                "warning: execution failed: {command}\nYou can fix the problem, and then run\n\n  \
                 alt rebase --continue\n\n"
            );
            return Response {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: said.into_bytes(),
            };
        }
        *JUST_RAN.lock().expect("not poisoned") = Some(command);
        response = on();
        *JUST_RAN.lock().expect("not poisoned") = None;
    }
}
