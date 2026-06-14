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
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("fatal: {e}");
            ExitCode::from(128)
        }
    }
}

fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;

    // Hot read commands route through the per-repo daemon to skip the ~21ms
    // store open; the daemon is auto-spawned if not already up. Local-first:
    // anything that can't (disabled, no repo, daemon unreachable, wire error)
    // falls through to the direct path below — the daemon is never required.
    #[cfg(unix)]
    if alt_cli::client::routes_through_daemon(&cli.command)
        && !alt_cli::client::disabled()
        && let Ok((alt_dir, _)) = native::resolve_workspace(&cwd, cli.workspace.as_deref())
        && let Some(resp) =
            alt_cli::client::try_serve(&alt_dir, &std::env::args().skip(1).collect::<Vec<_>>())
    {
        use std::io::Write as _;
        std::io::stdout().write_all(&resp.stdout)?;
        std::io::stderr().write_all(&resp.stderr)?;
        return Ok(resp.exit_code);
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
