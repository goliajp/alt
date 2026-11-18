//! `alt status`.

use alt_worktree::Untracked;

use super::*;

impl NativeRepo<'_> {
    /// The mode named `given`, or without one what `status.showUntrackedFiles`
    /// sets, which may be a boolean as for git; `normal` when nothing does.
    fn untracked_mode(&self, given: Option<&str>) -> Res<Untracked> {
        let invalid = |mode: &[u8]| {
            let mode = String::from_utf8_lossy(mode);
            format!("invalid untracked files mode '{mode}'")
        };
        if let Some(given) = given {
            return mode_named(given.as_bytes()).ok_or_else(|| invalid(given.as_bytes()).into());
        }
        let settings = self.settings()?;
        match settings.get("status", None, "showuntrackedfiles") {
            None => Ok(Untracked::Normal),
            // a key without a value is true
            Some(None) => Ok(Untracked::Normal),
            Some(Some(text)) => mode_named(text).ok_or_else(|| invalid(text).into()),
        }
    }

    /// `alt status`: staged / unstaged / untracked against HEAD and the index,
    /// plus any unmerged (conflicted) paths left by a merge. With `json`, emits
    /// the stable structured schema instead of the human view.
    ///
    /// `untracked` is git's `--untracked-files` mode: `no` leaves untracked
    /// files out, `normal` (the default) shows a directory with nothing
    /// tracked in it as one path ending in `/`, `all` shows every file.
    pub fn status(&self, untracked: Option<&str>, json: bool, out: &mut impl Write) -> Res<()> {
        let untracked = self.untracked_mode(untracked)?;
        let branch = self.head_branch()?;
        let state = self.tree_state(untracked)?;
        let conflicts = state.conflicts();
        let unmerged: std::collections::BTreeSet<BString> = conflicts.keys().cloned().collect();
        let mut st = state.status();
        // unmerged paths are reported in their own section, not as
        // staged/unstaged/untracked noise driven by the missing stage-0 entry
        st.staged.retain(|(p, _)| !unmerged.contains(p));
        st.unstaged.retain(|(p, _)| !unmerged.contains(p));
        st.untracked.retain(|p| !unmerged.contains(p));

        let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
        if json {
            return render_status_json(out, short, &self.id.principal, &st, &unmerged);
        }
        // as git shows them: from where the command was run, unless
        // `status.relativePaths` is off
        let settings = self.settings()?;
        let relative = settings.get_bool("status", None, "relativepaths");
        let prefix = match relative {
            Some(Ok(false)) => "",
            _ => self.prefix.as_str(),
        };
        writeln!(out, "On branch {short}")?;
        let quote = settings.get_bool("core", None, "quotepath").transpose()?;
        render_status(out, prefix, quote.unwrap_or(true), &st, &conflicts)
    }
}

/// One path of `status` after its `mark`: the bytes of its name as they
/// are, or quoted as git quotes them.
fn path_line(out: &mut impl Write, mark: &str, shown: &[u8], quote: bool) -> Res<()> {
    write!(out, "\t{mark}")?;
    crate::quote::write_path(out, shown, quote)?;
    Ok(out.write_all(b"\n")?)
}

/// The long form of `status`: its paths as they are named from `prefix`,
/// the conflicted ones after the staged ones, as for git.
fn render_status(
    out: &mut impl Write,
    prefix: &str,
    quote: bool,
    st: &alt_worktree::Status,
    conflicts: &std::collections::BTreeMap<BString, u8>,
) -> Res<()> {
    let shown = |p: &BString| pathspec::shown_from(prefix, p);
    let mark = |k: ChangeKind| match k {
        ChangeKind::Added => "new file:   ",
        ChangeKind::Modified => "modified:   ",
        ChangeKind::Deleted => "deleted:    ",
        ChangeKind::TypeChanged => "typechange: ",
    };
    if !st.staged.is_empty() {
        writeln!(out, "Changes to be committed:")?;
        for (p, k) in &st.staged {
            path_line(out, mark(*k), &shown(p), quote)?;
        }
    }
    if !conflicts.is_empty() {
        writeln!(out, "Unmerged paths:")?;
        for (p, stages) in conflicts {
            path_line(out, conflict_mark(*stages), &shown(p), quote)?;
        }
    }
    if !st.unstaged.is_empty() {
        writeln!(out, "Changes not staged for commit:")?;
        for (p, k) in &st.unstaged {
            path_line(out, mark(*k), &shown(p), quote)?;
        }
    }
    if !st.untracked.is_empty() {
        writeln!(out, "Untracked files:")?;
        for p in &st.untracked {
            path_line(out, "", &shown(p), quote)?;
        }
    }
    let changed = st.staged.len() + st.unstaged.len() + st.untracked.len();
    if conflicts.is_empty() && changed == 0 {
        writeln!(out, "nothing to commit, working tree clean")?;
    }
    Ok(())
}

/// What git calls a conflicted path by the stages the index has for it:
/// 1 for the base, 2 for ours, 4 for theirs. Padded to one width, as the
/// marks of the other sections are to theirs.
fn conflict_mark(stages: u8) -> &'static str {
    match stages {
        1 => "both deleted:    ",
        2 => "added by us:     ",
        3 => "deleted by them: ",
        4 => "added by them:   ",
        5 => "deleted by us:   ",
        6 => "both added:      ",
        _ => "both modified:   ",
    }
}

/// Renders `status` as the stable JSON schema (version 2):
/// `{schema_version, branch, principal:{kind,id,session?}, staged:[…],
/// unstaged:[…], untracked:[…], unmerged:[…], clean}`. `change` ∈
/// `added`/`modified`/`deleted`/`typechange`; version 1 had no
/// `typechange` and reported such a path as `modified`. A path that is
/// not UTF-8 has its bytes in hex beside it: as `path_bytes` in an entry,
/// in `untracked_bytes` and `unmerged_bytes` by its place in the list. `principal` is
/// the structured actor, so an agent can self-check "who am I to this
/// repo" via the same machine-first surface it uses for everything else.
fn render_status_json(
    out: &mut impl Write,
    branch: &str,
    principal: &Principal,
    st: &alt_worktree::Status,
    unmerged: &std::collections::BTreeSet<BString>,
) -> Res<()> {
    use crate::json::Json;
    let change = |k: ChangeKind| match k {
        ChangeKind::Added => "added",
        ChangeKind::Modified => "modified",
        ChangeKind::TypeChanged => "typechange",
        ChangeKind::Deleted => "deleted",
    };
    let entries = |v: &[(BString, ChangeKind)]| {
        Json::Array(
            v.iter()
                .map(|(p, k)| {
                    let mut entry = crate::json::path(p);
                    entry.push(("change", Json::str(change(*k))));
                    Json::Object(entry)
                })
                .collect(),
        )
    };
    let clean = unmerged.is_empty()
        && st.staged.is_empty()
        && st.unstaged.is_empty()
        && st.untracked.is_empty();
    let mut doc = vec![
        ("schema_version", Json::Num(2)),
        ("branch", Json::str(branch)),
        ("principal", principal_json(principal)),
        ("staged", entries(&st.staged)),
        ("unstaged", entries(&st.unstaged)),
    ];
    let untracked = st.untracked.iter().map(|path| path.as_slice());
    doc.extend(crate::json::paths(
        "untracked",
        "untracked_bytes",
        untracked,
    ));
    let unmerged = unmerged.iter().map(|path| path.as_slice());
    doc.extend(crate::json::paths("unmerged", "unmerged_bytes", unmerged));
    doc.push(("clean", Json::Bool(clean)));
    Json::Object(doc).write(out)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// The mode `text` names, by its name or as a boolean in one of the
/// spellings git takes: true is `normal`, false is `no`, and so is the
/// empty text.
fn mode_named(text: &[u8]) -> Option<Untracked> {
    match text.to_ascii_lowercase().as_slice() {
        b"" | b"no" | b"false" | b"off" | b"0" => Some(Untracked::No),
        b"normal" | b"true" | b"yes" | b"on" | b"1" => Some(Untracked::Normal),
        b"all" => Some(Untracked::All),
        _ => None,
    }
}
