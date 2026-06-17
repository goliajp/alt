//! `alt-web` daemon entry. Reads the `.alt` path + bind + worker count
//! from env, hands them to [`alt_web::router::serve`], blocks.

use std::path::PathBuf;
use std::process::ExitCode;

use alt_web::Source;

fn main() -> ExitCode {
    let bind = std::env::var("ALT_WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8091".to_string());
    let workers = std::env::var("ALT_WEB_WORKERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(2);
    let alt_dir = match std::env::var("ALT_WEB_REPO") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!("alt-web: ALT_WEB_REPO is required (path to the .alt directory to serve)");
            return ExitCode::from(2);
        }
    };

    let source = Source::new(alt_dir);
    if let Err(e) = alt_web::router::serve(&bind, source, workers) {
        eprintln!("alt-web: serve: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
