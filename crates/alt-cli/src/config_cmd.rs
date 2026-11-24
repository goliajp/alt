//! `alt config <key> [<value>] | --list | --unset <key>`: reads the
//! configuration that applies to the repository and writes the store's
//! own file, `<alt-dir>/git-import/config`. What an import kept of the
//! git repository's configuration lies under that file and is never
//! written here: a value set here stands over it, and the next import
//! leaves it standing.
//!
//! A write changes the lines of the one variable or section it is about,
//! as `git config` does: comments, blank lines and every other line of
//! the file stay, and a value is quoted and escaped so that it reads
//! back as it was given.

use std::io::Write;

use alt_git_config::Name;
use alt_repo::Repository;

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
        return list_all(&*crate::git_settings::repository(repo)?, out);
    }
    let Some(key) = key else {
        return Err("usage: alt config <key> [<value>] | --list | --unset <key>".into());
    };
    if let Some(new_value) = value {
        return set_one(repo, key, new_value, out);
    }
    let cfg = crate::git_settings::repository(repo)?;
    let (section, subsection, name) = split_key(key)?;
    let value = cfg
        .get_str(&section, subsection.as_deref(), &name)
        .ok_or_else(|| format!("'{key}' not set"))?;
    out.write_all(value.as_ref())?;
    out.write_all(b"\n")?;
    Ok(())
}

/// Refuses a variable that tells how the store is laid out: the hash of
/// its objects, the version of the format, an extension. The store has
/// it from where it was made, and what reads the store goes by it.
fn ensure_settable(key: &str) -> Res<()> {
    let (section, subsection, name) = split_key(key)?;
    let version = section == "core" && name == "repositoryformatversion";
    if subsection.is_none() && (section == "extensions" || version) {
        return Err(format!("'{key}' tells the format of the store and is not set by hand").into());
    }
    Ok(())
}

fn set_one(repo: &Repository, key: &str, value: &str, out: &mut impl Write) -> Res<()> {
    ensure_settable(key)?;
    set(repo, key, value)?;
    writeln!(out, "set {key} = {value}")?;
    Ok(())
}

/// Rewrites the store's own file as `edit` makes it of its text, under
/// the lock of the file; whether it was written.
fn rewrite(repo: &Repository, edit: impl FnOnce(&[u8]) -> Res<Option<Vec<u8>>>) -> Res<bool> {
    // for an alt-backed repo, git_dir() is the `.alt` dir
    let written = alt_repo::kept_config::rewrite_own(repo.git_dir(), edit)?;
    crate::git_settings::forget();
    Ok(written)
}

/// Sets `key` to `value` in the store's own file; of several values
/// the file has for it, the one given is left.
pub(crate) fn set(repo: &Repository, key: &str, value: &str) -> Res<()> {
    let (section, subsection, name) = split_key(key)?;
    let named = Name {
        section: &section,
        subsection: subsection.as_deref(),
        key: &name,
    };
    rewrite(repo, |file| {
        let entries = alt_git_config::parse_file(file)?;
        let several = entries.iter().filter(|e| Name::of(e) == named).count() > 1;
        let cleared = match several {
            true => alt_git_config::unset(file, named)?,
            false => None,
        };
        let file = cleared.as_deref().unwrap_or(file);
        Ok(Some(alt_git_config::set(file, named, value.as_bytes())?))
    })?;
    Ok(())
}

/// Both files a section of a branch is written in: the store's own, and
/// the copy an import kept of git's, which git would have written.
fn rewrite_both(repo: &Repository, edit: impl Fn(&[u8]) -> Res<Option<Vec<u8>>>) -> Res<()> {
    rewrite(repo, &edit)?;
    alt_repo::kept_config::rewrite_copy(repo.git_dir(), &edit)?;
    crate::git_settings::forget();
    Ok(())
}

/// Takes what is set in `[<section> "<subsection>"]` out of the store's
/// own file and out of the copy of git's.
pub(crate) fn drop_section(repo: &Repository, section: &str, subsection: &str) -> Res<()> {
    rewrite_both(repo, |file| {
        let removed = alt_git_config::remove_section(file, section, subsection.as_bytes());
        Ok(removed?)
    })
}

/// Has what is set in `[<section> "<from>"]` set in `[<section> "<to>"]`
/// instead, in the store's own file and in the copy of git's; what was
/// set for `to` goes from both.
pub(crate) fn rename_section(repo: &Repository, section: &str, from: &str, to: &str) -> Res<()> {
    let (from, to) = (from.as_bytes(), to.as_bytes());
    rewrite_both(repo, |file| {
        let cleared = alt_git_config::remove_section(file, section, to)?;
        let renamed = {
            let file = cleared.as_deref().unwrap_or(file);
            alt_git_config::rename_section(file, section, from, to)?
        };
        Ok(renamed.or(cleared))
    })
}

fn unset_one(repo: &Repository, key: &str, out: &mut impl Write) -> Res<()> {
    ensure_settable(key)?;
    let (section, subsection, name) = split_key(key)?;
    let named = Name {
        section: &section,
        subsection: subsection.as_deref(),
        key: &name,
    };
    if !rewrite(repo, |file| Ok(alt_git_config::unset(file, named)?))? {
        let applies = crate::git_settings::repository(repo)?;
        if applies
            .get(&section, subsection.as_deref(), &name)
            .is_some()
        {
            let whose = "is set by the imported git configuration, which stays as it is";
            return Err(format!("'{key}' {whose}; `alt config {key} <value>` overrides it").into());
        }
        return Err(format!("'{key}' not set").into());
    }
    writeln!(out, "unset {key}")?;
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
/// A leading or trailing dot, or fewer than two segments, is an error,
/// and so is what git takes for no key: a section or a name with
/// anything but letters, digits and `-` in it, a name that begins with
/// no letter, a subsection with a newline in it.
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
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'-';
    let named = section.bytes().all(word)
        && name.bytes().all(word)
        && name.as_bytes()[0].is_ascii_alphabetic();
    let breaks = |sub: &Vec<u8>| sub.iter().any(|b| matches!(b, b'\n' | 0));
    if !named || subsection.as_ref().is_some_and(breaks) {
        return Err(format!("bad config key '{}'", key.escape_default()).into());
    }
    Ok((section, subsection, name))
}
