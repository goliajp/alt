//! `alt-web` daemon entry. Reads the multi-repo root + bind + worker
//! count from env, hands them to [`alt_web::router::serve`], blocks.

use std::path::PathBuf;
use std::process::ExitCode;

use alt_web::MultiRepo;

fn main() -> ExitCode {
    let bind = std::env::var("ALT_WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8091".to_string());
    let workers = std::env::var("ALT_WEB_WORKERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(2);
    let root = match std::env::var("ALT_WEB_ROOT") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!(
                "alt-web: ALT_WEB_ROOT is required (path to the multi-repo root holding <name>/.alt subdirs)"
            );
            return ExitCode::from(2);
        }
    };

    let mr = MultiRepo::new(root);
    if let Err(e) = alt_web::router::serve(&bind, mr, workers) {
        eprintln!("alt-web: serve: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
