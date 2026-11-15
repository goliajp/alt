//! Direct and daemon commands share parsing, failures, and durable receipts.
//!
//! The C runtime calls `main` directly. The standard library's own start-up
//! finds the main thread's stack guard by reading `/proc/self/maps` and
//! probes the standard descriptors, about twenty system calls on every
//! invocation; what a command relies on (a closed pipe as a write error, a
//! report when the stack overflows) is set up here in four.
#![cfg_attr(not(test), no_main)]

use alt_cli::cli::{self, Command};
use alt_cli::daemon::Response;
use alt_cli::execution::Execution;
use alt_cli::machine::{self, Failure};
use alt_cli::native::{self, Identity, Store};
use alt_repo::Repository;
use std::io::Write;

#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn main(
    _argc: std::ffi::c_int,
    _argv: *const *const std::ffi::c_char,
) -> std::ffi::c_int {
    // a closed output pipe is reported as a failed write, not a signal
    alt_os::sys::ignore_signal(alt_os::sys::SIGPIPE);
    alt_os::stack::report_overflow();
    // a panic leaves with the status the standard runtime gives it
    let Ok(response) = std::panic::catch_unwind(run) else {
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
    let mut args = std::env::args_os().collect::<Vec<_>>();
    let mut parsed = match machine::parse(args.clone()) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
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
    if let Command::Runner { op, .. } = &parsed.cli.command {
        if parsed.cli.request_id.is_some() || parsed.cli.workspace.is_some() {
            return Failure::usage("runner uses its snapshot nonce; repository request/workspace selectors are not supported")
                .response(parsed.json, parsed.cli.request_id.as_deref());
        }
        let mut stdout = Vec::new();
        return match alt_cli::runner::run(op, &mut stdout) {
            Ok(()) => Response {
                exit_code: 0,
                stdout,
                stderr: Vec::new(),
            },
            Err(error) => machine::from_error(error.as_ref(), false).response(parsed.json, None),
        };
    }
    if let Command::Auth { op, .. } = &parsed.cli.command {
        if parsed.cli.request_id.is_some() || parsed.cli.workspace.is_some() {
            return Failure::usage("auth uses a private login journal; repository request/workspace selectors are not supported").response(parsed.json,None);
        }
        let mut stdout = Vec::new();
        return match alt_cli::auth::run(op, &mut stdout, &mut std::io::stderr().lock()) {
            Ok(()) => Response {
                exit_code: 0,
                stdout,
                stderr: Vec::new(),
            },
            Err(error) => machine::from_error(error.as_ref(), false).response(parsed.json, None),
        };
    }
    if let Command::Portable { op, .. } = &parsed.cli.command {
        if parsed.cli.request_id.is_some() || parsed.cli.workspace.is_some() {
            return Failure::usage("portable transfer uses its private journal; repository request/workspace selectors are not supported").response(parsed.json,None);
        }
        let mut stdout = Vec::new();
        return match alt_cli::portable::run(op, &mut stdout) {
            Ok(()) => Response {
                exit_code: 0,
                stdout,
                stderr: Vec::new(),
            },
            Err(error) => machine::from_error(error.as_ref(), false).response(parsed.json, None),
        };
    }
    let identity = Identity::from_env();
    let native_write =
        cli::is_native(&parsed.cli.command) && cli::mutates_command(&parsed.cli.command);
    if native_write {
        let id = match parsed.cli.request_id.as_deref() {
            Some(id) => match alt_cli::request::parse_id(id) {
                Ok(id) => id,
                Err(error) => return error.response(parsed.json, Some(id)),
            },
            None => match alt_cli::request::new_id() {
                Ok(id) => id,
                Err(error) => {
                    return machine::from_error(&error, false).response(parsed.json, None);
                }
            },
        };
        if parsed.cli.request_id.is_none() {
            args.splice(1..1, ["--request-id".into(), machine::hex(&id).into()]);
            parsed = match machine::parse(args.clone()) {
                Ok(parsed) => parsed,
                Err(response) => return response,
            };
        }
        let submitted = alt_cli::execution::binding(&parsed, &cwd, &identity)
            .and_then(|(dir, _, _, value)| alt_cli::request::record_submission(&dir, id, &value));
        if let Err(error) = submitted {
            return machine::from_error(error.as_ref(), false)
                .response(parsed.json, Some(&machine::hex(&id)));
        }
    }
    #[cfg(unix)]
    if alt_cli::client::routes_through_daemon(&parsed.cli.command)
        && !alt_cli::client::disabled()
        && cwd.to_str().is_some()
        && let Ok((alt_dir, _)) = native::resolve_workspace(&cwd, parsed.cli.workspace.as_deref())
    {
        use alt_cli::client::Outcome;
        let argv = args
            .iter()
            .skip(1)
            .map(|s| s.to_str().expect("validated arguments").to_owned())
            .collect::<Vec<_>>();
        let outcome = if native_write {
            let id =
                alt_cli::request::parse_id(parsed.cli.request_id.as_deref().expect("write id"))
                    .expect("validated id");
            alt_cli::client::serve_write(&alt_dir, &argv, id)
        } else {
            alt_cli::client::try_serve(&alt_dir, &argv)
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
    let execution = match Execution::prepare(parsed, cwd.clone(), identity, None) {
        Ok(execution) => execution,
        Err(response) => return response,
    };
    if cli::is_native(&execution.parsed.cli.command) {
        let opened = (|| -> Result<_, Box<dyn std::error::Error>> {
            let (dir, coord) =
                native::resolve_workspace(&cwd, execution.parsed.cli.workspace.as_deref())?;
            let store = Store::open(dir.clone())?;
            let repo = Repository::open_alt(dir, Some(coord.root().to_owned()))?;
            Ok((store, repo))
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
                    store.sink()?.fsync_all()
                } else {
                    Ok(())
                }
            });
        if let Err(error) = barrier {
            return Failure::unknown(format!("durability: {error}")).response(
                execution.parsed.json,
                execution.parsed.cli.request_id.as_deref(),
            );
        }
        return execution.publish(response, operations);
    }
    let capture = alt_cli::diagnostics::Capture::start(execution.parsed.json);
    let mut out = Vec::new();
    let result = (|| -> Result<u8, Box<dyn std::error::Error>> {
        match &execution.parsed.cli.command {
            Command::Init { dir } => native::init(dir.clone(), &mut out)?,
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
                &Repository::discover(&cwd)?,
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
                let (repo, head) = match native::resolve_workspace(
                    &cwd,
                    execution.parsed.cli.workspace.as_deref(),
                ) {
                    Ok((dir, coord)) => (
                        Repository::open_alt(dir, Some(coord.root().to_owned()))?,
                        coord.head_ref().to_owned(),
                    ),
                    Err(error) if execution.parsed.cli.workspace.is_some() => return Err(error),
                    Err(_) => (Repository::discover(&cwd)?, "HEAD".to_owned()),
                };
                cli::run_git(&repo, command, &head, &mut out)?;
            }
        }
        Ok(0)
    })();
    execution.finish(result, out, capture.finish(), &[])
}
