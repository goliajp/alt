//! `alt config <key> | --list`: read-only access to the effective git
//! config the repo sees (merged includes + per-repo + globals).
//!
//! Write support (`git config <key> <value>`, `--global`, `--unset`,
//! …) is intentionally out of scope for this commit — the config
//! file is a non-trivial format with comment + whitespace
//! preservation requirements, and writes would need to round-trip
//! cleanly. Users who need to *set* values can still go through
//! `git config` or edit `~/.gitconfig` directly; alt reads the same
//! files.

use std::io::Write;

use alt_repo::Repository;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

pub fn run(repo: &Repository, key: Option<&str>, list: bool, out: &mut impl Write) -> Res<()> {
    let cfg = repo.config();
    if list {
        return list_all(cfg, out);
    }
    let Some(key) = key else {
        return Err("usage: alt config <key> | --list".into());
    };
    let (section, subsection, name) = split_key(key)?;
    let value = cfg
        .get_str(&section, subsection.as_deref(), &name)
        .ok_or_else(|| format!("'{key}' not set"))?;
    out.write_all(value.as_ref())?;
    out.write_all(b"\n")?;
    Ok(())
}

fn list_all(cfg: &alt_git_config::Config, out: &mut impl Write) -> Res<()> {
    for entry in &cfg.entries {
        let key = entry.display_key();
        match &entry.value {
            Some(v) => {
                write!(out, "{key}=")?;
                out.write_all(v.as_ref())?;
                out.write_all(b"\n")?;
            }
            None => writeln!(out, "{key}")?,
        }
    }
    Ok(())
}

/// Parse a dotted git config key. Two forms:
/// - `section.name` (no subsection)
/// - `section.subsection.name` (the middle part is the subsection,
///   case-preserved)
///
/// A leading or trailing dot, or fewer than two segments, is an error.
fn split_key(key: &str) -> Res<(String, Option<Vec<u8>>, String)> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.len() < 2 || parts.iter().any(|p| p.is_empty()) {
        return Err(format!("bad config key '{key}'").into());
    }
    let section = parts[0].to_lowercase();
    let name = parts[parts.len() - 1].to_lowercase();
    let subsection = if parts.len() > 2 {
        // Subsections may be multi-segment if they contain dots; rejoin
        // the middle parts in their original case.
        let middle = parts[1..parts.len() - 1].join(".");
        Some(middle.into_bytes())
    } else {
        None
    };
    Ok((section, subsection, name))
}
