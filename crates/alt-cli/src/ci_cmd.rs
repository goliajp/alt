//! The `alt ci` command.

use std::io::Write;
use std::path::Path;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// `alt ci validate` — load every requested workflow.toml (or scan
/// `.alt/ci/*/workflow.toml` when `paths` is empty), run the
/// parser + linter, and print one diagnostic per line. Returns a
/// non-zero exit code when any diagnostic is an error; warnings alone
/// still exit 0. Repository-agnostic: this is a file-tier lint, not a
/// store operation, so `alt ci validate` works in any directory.
pub fn ci_validate<W: Write>(
    cwd: &Path,
    paths: &[std::path::PathBuf],
    json: bool,
    out: &mut W,
) -> Res<u8> {
    use crate::ci;

    let files: Vec<std::path::PathBuf> = if paths.is_empty() {
        let mut found = Vec::new();
        let ci_dir = cwd.join(".alt").join("ci");
        if let Ok(read) = std::fs::read_dir(&ci_dir) {
            for entry in read.flatten() {
                let candidate = entry.path().join("workflow.toml");
                if candidate.is_file() {
                    found.push(candidate);
                }
            }
            found.sort();
        }
        if found.is_empty() {
            writeln!(
                out,
                "no workflow.toml files found under .alt/ci/<name>/workflow.toml"
            )?;
            return Ok(0);
        }
        found
    } else {
        paths.to_vec()
    };

    let mut error_count = 0usize;
    for file in &files {
        let display = file.display().to_string();
        let src = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                if json {
                    let row = crate::json::Json::Object(vec![
                        ("file", crate::json::Json::str(&display)),
                        ("severity", crate::json::Json::str("error")),
                        (
                            "message",
                            crate::json::Json::str(format!("cannot read: {e}")),
                        ),
                    ]);
                    row.write(out)?;
                    writeln!(out)?;
                } else {
                    writeln!(out, "{display}: error: cannot read: {e}")?;
                }
                error_count += 1;
                continue;
            }
        };
        let (wf, mut diags) = ci::parse_workflow(&src);
        diags.extend(ci::lint(&wf));
        for diag in diags {
            let (line, col) = diag.at.resolve(&src);
            if diag.severity == ci::Severity::Error {
                error_count += 1;
            }
            if json {
                let row = crate::json::Json::Object(vec![
                    ("file", crate::json::Json::str(&display)),
                    ("line", crate::json::Json::Num(line as i64)),
                    ("col", crate::json::Json::Num(col as i64)),
                    ("severity", crate::json::Json::str(diag.severity.as_str())),
                    ("message", crate::json::Json::str(&diag.message)),
                ]);
                row.write(out)?;
                writeln!(out)?;
            } else {
                writeln!(
                    out,
                    "{display}:{line}:{col}: {sev}: {msg}",
                    sev = diag.severity.as_str(),
                    msg = diag.message
                )?;
            }
        }
    }
    Ok(if error_count > 0 { 1 } else { 0 })
}
