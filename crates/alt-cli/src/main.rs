//! Direct and daemon commands share parsing, failures, and durable receipts.
//!
//! The C runtime calls `main` directly. The standard library's own start-up
//! finds the main thread's stack guard by reading `/proc/self/maps` and
//! probes the standard descriptors, about twenty system calls on every
//! invocation; what a command relies on (a closed pipe as a write error, a
//! report when the stack overflows, `/dev/null` on a standard descriptor
//! that was closed) is set up here in five.
#![cfg_attr(not(test), no_main)]

// the unwinder is linked in rather than loaded: one shared library fewer
// to map and bind at every start
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[link(name = "gcc_eh", kind = "static")]
unsafe extern "C" {}

use alt_cli::cli::{self, Command};
use alt_cli::daemon::Response;
use alt_cli::execution::{Execution, submit};
use alt_cli::machine::{self, Failure};
use alt_cli::native::pathspec::Place;
use alt_cli::native::{self, Identity};
use alt_repo::Repository;
use std::ffi::OsString;
use std::io::Write;

#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn main(
    _argc: std::ffi::c_int,
    _argv: *const *const std::ffi::c_char,
) -> std::ffi::c_int {
    // before anything is opened: a file must never become standard output
    alt_os::stdio::fill_closed();
    alt_os::heap::keep_freed_memory();
    // a closed output pipe is reported as a failed write, not a signal
    alt_os::sys::ignore_signal(alt_os::sys::SIGPIPE);
    alt_os::stack::report_overflow();
    // a panic leaves with the status the standard runtime gives it, and
    // with whatever was printed before it
    let Ok(response) = std::panic::catch_unwind(run) else {
        let _ = std::io::stdout().flush();
        return 101;
    };
    deliver(&response).into()
}

/// The test harness brings its own entry point.
#[cfg(test)]
fn main() -> std::process::ExitCode {
    deliver(&run()).into()
}

/// Writes a command's output and gives its exit status.
fn deliver(response: &Response) -> u8 {
    // nothing flushes standard output after the C `main` returns
    let delivered = (|| {
        let mut out = std::io::stdout().lock();
        out.write_all(&response.stdout)?;
        out.flush()?;
        std::io::stderr().lock().write_all(&response.stderr)
    })();
    if delivered.is_ok() {
        response.exit_code
    } else {
        75
    }
}

fn run() -> Response {
    let args = alt_cli::precompose::argv(std::env::args_os().collect());
    let response = match machine::parse(args.clone()) {
        Ok(parsed) => match alt_cli::execution::for_shell(&parsed.cli.command) {
            Some(respond) => return respond(Box::new(|| attempt(args, parsed))),
            None => {
                let (json, on) = (parsed.json, go_on(&parsed));
                let first = attempt(args, parsed);
                alt_cli::exec_step::run(first, json, &mut || match machine::parse(on.clone()) {
                    Ok(parsed) => attempt(on.clone(), parsed),
                    Err(response) => response,
                })
            }
        },
        Err(response) => response,
    };
    alt_cli::past_damage::report(native::tell_left_out(response))
}

/// The command line that goes on with the rebase `parsed` asks for.
fn go_on(parsed: &machine::Parsed) -> Vec<OsString> {
    let mut words = vec!["alt"];
    if let Some(workspace) = &parsed.cli.workspace {
        words.extend(["--workspace", workspace]);
    }
    words.extend(["rebase", "--continue"]);
    if parsed.json {
        words.push("--json");
    }
    words.into_iter().map(OsString::from).collect()
}

fn attempt(mut args: Vec<OsString>, mut parsed: machine::Parsed) -> Response {
    if let Err(refused) = alt_cli::past_damage::admit(&parsed.cli.command) {
        return refused.response(parsed.json, None);
    }
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => return machine::from_error(&error, false).response(parsed.json, None),
    };
    if cwd.to_str().is_none() {
        let mut error = Failure::usage("non-UTF-8 working directories are not supported");
        error.kind = "unsupported_path";
        *error.details = alt_json::json!({"path_bytes_hex": machine::hex(cwd.as_os_str().as_encoded_bytes()), "encoding":"os_bytes"});
        return error.response(parsed.json, parsed.cli.request_id.as_deref());
    }
    let own: Option<(&str, alt_cli::execution::OwnRun)> = match &parsed.cli.command {
        Command::Runner { op, .. } => Some((
            "runner uses its snapshot nonce; repository request/workspace selectors are not supported",
            &|out| alt_cli::runner::run(op, out),
        )),
        Command::Auth { op, .. } => Some((
            "auth uses a private login journal; repository request/workspace selectors are not supported",
            &|out| alt_cli::auth::run(op, out, &mut std::io::stderr().lock()),
        )),
        Command::Portable { op, .. } => Some((
            "portable transfer uses its private journal; repository request/workspace selectors are not supported",
            &|out| alt_cli::portable::run(op, out),
        )),
        _ => None,
    };
    if let Some((refused, run)) = own {
        return alt_cli::execution::outside_repository(&parsed, refused, run);
    }
    let (forwarded, identity) = Identity::of_process();
    let native_write =
        cli::is_native(&parsed.cli.command) && cli::mutates_command(&parsed.cli.command);
    let (mut drawn, mut bound) = (false, None);
    if native_write {
        (parsed, bound, drawn) = match submit(&mut args, parsed, &cwd, &identity) {
            Ok((parsed, bound, drawn)) => (parsed, Some(bound), drawn),
            Err(response) => return response,
        };
    }
    #[cfg(unix)]
    if alt_cli::client::routes_through_daemon(&parsed.cli.command)
        && !alt_cli::client::disabled()
        && let Ok((alt_dir, _)) = native::resolve_workspace(&cwd, parsed.cli.workspace.as_deref())
    {
        use alt_cli::client::Outcome;
        let words = args.iter().skip(1);
        let mut request = alt_cli::daemon::Request {
            args: (words.map(|s| s.to_str().expect("validated arguments").to_owned())).collect(),
            cwd: cwd.clone(),
            env: forwarded,
            ..Default::default()
        };
        let outcome = if native_write {
            let id = parsed.cli.request_id.as_deref().expect("write id");
            (request.id, request.drawn) = (alt_cli::request::parse_id(id).ok(), drawn);
            alt_cli::client::serve_write(&alt_dir, &request)
        } else {
            alt_cli::client::try_serve(&alt_dir, &request)
        };
        match outcome {
            Outcome::Served(response) => {
                if parsed.json
                    && response.exit_code != 0
                    && alt_json::from_slice::<alt_json::Value>(&response.stderr)
                        .ok()
                        .and_then(|v| v["error"]["outcome"].as_str().map(str::to_owned))
                        .is_none()
                {
                    let mut error =
                        Failure::unknown("daemon did not return a supported command result");
                    *error.details = alt_json::json!({"stdout":machine::bytes_value(&response.stdout),"stderr":machine::bytes_value(&response.stderr)});
                    return error.response(true, parsed.cli.request_id.as_deref());
                }
                return response;
            }
            Outcome::LostAfterSend if native_write => {
                return Failure::unknown(
                    "daemon response was lost; query this request before any new write",
                )
                .response(parsed.json, parsed.cli.request_id.as_deref());
            }
            Outcome::NotSent | Outcome::LostAfterSend => {}
        }
    }
    let execution = match Execution::prepare(parsed, cwd.clone(), identity, None, drawn, bound) {
        Ok(execution) => execution,
        Err(response) => return response,
    };
    if cli::is_native(&execution.parsed.cli.command) {
        return on_native_store(execution, &cwd);
    }
    let capture = alt_cli::diagnostics::Capture::start(execution.parsed.json);
    let mut out = Vec::new();
    let result = (|| -> Result<u8, Box<dyn std::error::Error>> {
        match &execution.parsed.cli.command {
            Command::Init(args) => native::init(args.dir.clone(), &mut out)?,
            Command::Clone { url, dir, json } => native::clone(
                url,
                dir.as_deref(),
                *json,
                &cwd,
                execution.identity.clone(),
                &mut out,
            )?,
            Command::Intake {
                source,
                target,
                git,
                json,
            } => native::task_intake(source, target, git, *json, &mut out)?,
            Command::Import { target } => native::import(
                &native::discover_owned(&cwd)?,
                target,
                &cwd,
                execution.identity.clone(),
                &mut out,
            )?,
            Command::Ci {
                op: cli::CiOp::Validate { paths },
                json,
            } => return alt_cli::ci_cmd::ci_validate(&cwd, paths, *json, &mut out),
            command => {
                let named = execution.parsed.cli.workspace.as_deref();
                let (repo, at) = match native::resolve_workspace(&cwd, named) {
                    Ok((dir, coord)) => (
                        Repository::open_alt(dir, Some(coord.root().to_owned()))?,
                        Place::at(coord),
                    ),
                    Err(error) if named.is_some() => return Err(error),
                    Err(_) => {
                        let repo = native::discover_owned(&cwd)?;
                        let at = Place::in_git(&repo, &cwd)?;
                        (repo, at)
                    }
                };
                let repo = native::view_for(repo, command);
                cli::run_git(&repo, command, &at, &mut out)?;
                if !execution.writing {
                    leave_to_exit(repo);
                }
            }
        }
        Ok(0)
    })();
    let response = execution.finish(result, out, capture.finish(), &[]);
    if !execution.writing && response.exit_code == 0 {
        leave_to_exit(execution);
    }
    response
}

/// Leaves what a command that only read holds for the exit of the process
/// to take down, which follows as soon as the answer is written: closing
/// the files, unmapping and freeing one by one is work the exit does at
/// once. Not for a handle that may have appended to a log, which syncs it
/// when dropped: a [`Repository`] has no way to append, and an
/// [`Execution`] of a command that does not write holds no file.
fn leave_to_exit<T>(held: T) {
    std::mem::forget(held);
}

/// Runs a command of the native store in this process: opens the store,
/// runs the command, and publishes its result once what it wrote is synced.
fn on_native_store(execution: Execution, cwd: &std::path::Path) -> Response {
    let opened = (|| -> Result<_, Box<dyn std::error::Error>> {
        let (dir, coord) =
            native::resolve_workspace(cwd, execution.parsed.cli.workspace.as_deref())?;
        native::open_for_command(&execution.parsed.cli.command, dir, coord.root())
    })();
    let (mut store, repo) = match opened {
        Ok(pair) => pair,
        Err(error) => return execution.failure(error.as_ref()),
    };
    let (response, operations) = execution.on_store(&mut store, &repo);
    let barrier = store
        .odb_mut()
        .flush_pending()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        .and_then(|()| {
            if execution.writing {
                store.sync_held()
            } else {
                Ok(())
            }
        });
    if let Err(error) = barrier {
        let causes = machine::causes(error.as_ref());
        return Failure::unknown(format!("durability: {causes}")).response(
            execution.parsed.json,
            execution.parsed.cli.request_id.as_deref(),
        );
    }
    execution.publish(response, operations)
}
