//! `alt config <key> [<value>] | --list | --unset <key>`: read + write
//! access to the alt repo's config (the same `<alt-dir>/git-import/
//! config` file `alt-import` preserved from the source `.git` so the
//! existing git config story keeps working post-transition).
//!
//! Write caveat: the on-disk file is rewritten from the parsed Entry
//! list, which **does not preserve comments, blank lines, or include
//! directives** — git's own implementation does sub-line edits to keep
//! those, which is a much heavier piece. For a dogfood first pass we
//! accept the lossiness; users who care about preserving comments
//! can edit the file by hand or stay on `git config`.

use std::io::Write;
use std::path::PathBuf;

use alt_git_config::Entry;
use alt_repo::Repository;
use bstr::BString;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

pub fn run(
    repo: &Repository,
    key: Option<&str>,
    value: Option<&str>,
    list: bool,
    unset: Option<&str>,
    out: &mut impl Write,
) -> Res<()> {
    if let Some(unset_key) = unset {
        return unset_one(repo, unset_key, out);
    }
    if list {
        return list_all(repo.config(), out);
    }
    let Some(key) = key else {
        return Err("usage: alt config <key> [<value>] | --list | --unset <key>".into());
    };
    if let Some(new_value) = value {
        return set_one(repo, key, new_value, out);
    }
    let cfg = repo.config();
    let (section, subsection, name) = split_key(key)?;
    let value = cfg
        .get_str(&section, subsection.as_deref(), &name)
        .ok_or_else(|| format!("'{key}' not set"))?;
    out.write_all(value.as_ref())?;
    out.write_all(b"\n")?;
    Ok(())
}

fn set_one(repo: &Repository, key: &str, value: &str, out: &mut impl Write) -> Res<()> {
    let (section, subsection, name) = split_key(key)?;
    let path = config_path(repo);
    let mut entries = current_entries(&path)?;

    let mut replaced = false;
    for e in entries.iter_mut() {
        if e.section == section
            && entry_subsection_bytes(e) == subsection.as_deref()
            && e.key == name
        {
            e.value = Some(BString::from(value.as_bytes()));
            replaced = true;
        }
    }
    if !replaced {
        entries.push(Entry {
            section: section.clone(),
            subsection: subsection.as_ref().map(|s| BString::from(s.clone())),
            key: name.clone(),
            value: Some(BString::from(value.as_bytes())),
        });
    }
    write_entries(&path, &entries)?;
    writeln!(out, "set {key} = {value}")?;
    Ok(())
}

fn unset_one(repo: &Repository, key: &str, out: &mut impl Write) -> Res<()> {
    let (section, subsection, name) = split_key(key)?;
    let path = config_path(repo);
    let mut entries = current_entries(&path)?;
    let before = entries.len();
    entries.retain(|e| {
        !(e.section == section
            && entry_subsection_bytes(e) == subsection.as_deref()
            && e.key == name)
    });
    if entries.len() == before {
        return Err(format!("'{key}' not set").into());
    }
    write_entries(&path, &entries)?;
    writeln!(out, "unset {key}")?;
    Ok(())
}

fn entry_subsection_bytes(e: &Entry) -> Option<&[u8]> {
    e.subsection.as_ref().map(|b| b.as_slice())
}

fn config_path(repo: &Repository) -> PathBuf {
    // For an alt-backed repo, git_dir() == the `.alt` dir, and the
    // config alt-import preserves lives under git-import/config.
    repo.git_dir().join("git-import").join("config")
}

fn current_entries(path: &PathBuf) -> Res<Vec<Entry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read(path)?;
    Ok(alt_git_config::parse_file(&data)?)
}

fn write_entries(path: &PathBuf, entries: &[Entry]) -> Res<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serialize_entries(entries);
    let tmp = path.with_extension("config.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn serialize_entries(entries: &[Entry]) -> Vec<u8> {
    use std::io::Write as _;
    let mut out = Vec::new();
    let mut last_section: Option<(String, Option<BString>)> = None;
    for e in entries {
        let section_key = (e.section.clone(), e.subsection.clone());
        if last_section.as_ref() != Some(&section_key) {
            match &e.subsection {
                Some(sub) => {
                    writeln!(out, "[{} \"{}\"]", e.section, escape_subsection(sub)).unwrap()
                }
                None => writeln!(out, "[{}]", e.section).unwrap(),
            }
            last_section = Some(section_key);
        }
        match &e.value {
            Some(v) => {
                out.extend_from_slice(b"\t");
                out.extend_from_slice(e.key.as_bytes());
                out.extend_from_slice(b" = ");
                out.extend_from_slice(v.as_ref());
                out.extend_from_slice(b"\n");
            }
            None => writeln!(out, "\t{}", e.key).unwrap(),
        }
    }
    out
}

/// Escape a subsection name for the `[section "<sub>"]` header form.
/// git rules: backslashes and double-quotes are escaped; everything
/// else is verbatim. Subsection names in practice are ASCII paths
/// (branch / remote / submodule names), so a lossy utf-8 view is fine
/// for the escape pass.
fn escape_subsection(sub: &BString) -> String {
    let s = String::from_utf8_lossy(sub);
    s.replace('\\', "\\\\").replace('"', "\\\"")
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
