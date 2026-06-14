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
    let stdout = std::io::stdout().lock();
    let mut out = std::io::BufWriter::new(stdout);
    let cwd = std::env::current_dir()?;
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
