//! The `alt` CLI binary. Parses the command line and dispatches: native `.alt`
//! commands against a freshly opened store, git-layer commands against a
//! discovered repository, `init` creates a new repo. The daemon (`altd`) reuses
//! the same per-command dispatch in `alt_cli::cli`, against a store it holds
//! open across requests.

use std::io::Write;
use std::process::ExitCode;

use alt_cli::cli::{self, Cli, Command};
use alt_cli::native::{self, Identity, OpenRepo};
use alt_repo::Repository;
use clap::Parser;

fn main() -> ExitCode {
    // Cheap pre-parse so a parse / config error before clap runs is still
    // reported in the right shape; clap's own errors stay structured by clap.
    let json_mode = std::env::args().any(|a| a == "--json");
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let msg = format!("{e}");
            // C4: A6 gate denials surface as JSON on stderr when the command
            // was invoked with `--json`, so an agent driving alt never has to
            // parse the human "fatal: …" string to know it was denied.
            if json_mode && let Some(rest) = msg.strip_prefix("capability denied: ") {
                eprintln!(
                    "{{\"schema_version\":1,\"error\":{{\"kind\":\"capability_denied\",\"message\":{}}}}}",
                    json_str(rest)
                );
                ExitCode::from(1)
            } else {
                eprintln!("fatal: {msg}");
                ExitCode::from(128)
            }
        }
    }
}

/// Compact JSON-string-literal encoder for the one error message field above;
/// keeps `main` from pulling in a JSON helper just to render an error.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x00..=0x1f => out.push_str(&format!("\\u{b:04x}")),
            _ => out.push(b as char),
        }
    }
    out.push('"');
    out
}

fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;

    // Hot commands route through the per-repo daemon to skip the store open;
    // the daemon is auto-spawned if not already up. Local-first: anything the
    // daemon can't serve falls through to the direct path below. A read is
    // idempotent (re-runs harmlessly on any failure). A write is exactly-once:
    // it carries an idempotency id and `serve_write` retries with that id on a
    // lost response (the daemon dedups a completed write, durably). Only a
    // first-attempt `NotSent` falls back to a direct run; a write whose request
    // had gone out and never got a response surfaces an error after the retry
    // budget rather than risk a double run.
    #[cfg(unix)]
    if alt_cli::client::routes_through_daemon(&cli.command)
        && !alt_cli::client::disabled()
        && let Ok((alt_dir, _)) = native::resolve_workspace(&cwd, cli.workspace.as_deref())
    {
        use alt_cli::client::Outcome;
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        let idempotent = alt_cli::client::is_idempotent(&cli.command);
        let outcome = if idempotent {
            alt_cli::client::try_serve(&alt_dir, &args)
        } else {
            alt_cli::client::serve_write(&alt_dir, &args)
        };
        match outcome {
            Outcome::Served(resp) => {
                use std::io::Write as _;
                std::io::stdout().write_all(&resp.stdout)?;
                std::io::stderr().write_all(&resp.stderr)?;
                return Ok(resp.exit_code);
            }
            Outcome::LostAfterSend if !idempotent => {
                return Err(
                    "the daemon connection dropped after the command was sent and \
                    repeated retries did not reconnect; it may have already taken effect — \
                    check with `alt status` / `alt log` before retrying"
                        .into(),
                );
            }
            // NotSent (daemon never acted), or a lost response for an idempotent
            // read — fall through and run the command directly
            Outcome::NotSent | Outcome::LostAfterSend => {}
        }
    }

    let stdout = std::io::stdout().lock();
    let mut out = std::io::BufWriter::new(stdout);
    let id = Identity::from_env();

    let code = match &cli.command {
        Command::Init { dir } => {
            native::init(dir.clone(), &mut out)?;
            0
        }
        c if cli::is_native(c) => {
            let mut open = OpenRepo::discover(&cwd, cli.workspace.as_deref(), id)?;
            cli::run_native(&mut open.repo(), c, &mut out)?
        }
        c => {
            let repo = Repository::discover(&cwd)?;
            cli::run_git(&repo, c, &mut out)?;
            0
        }
    };
    out.flush()?;
    Ok(code)
}
