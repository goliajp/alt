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
        let unmerged = state.unmerged();
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
        writeln!(out, "On branch {short}")?;
        let mark = |k: ChangeKind| match k {
            ChangeKind::Added => "new file",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
        };
        if !unmerged.is_empty() {
            writeln!(out, "Unmerged paths:")?;
            for p in &unmerged {
                writeln!(out, "\tboth modified:   {p}")?;
            }
        }
        if !st.staged.is_empty() {
            writeln!(out, "Changes to be committed:")?;
            for (p, k) in &st.staged {
                writeln!(out, "\t{}:   {p}", mark(*k))?;
            }
        }
        if !st.unstaged.is_empty() {
            writeln!(out, "Changes not staged for commit:")?;
            for (p, k) in &st.unstaged {
                writeln!(out, "\t{}:   {p}", mark(*k))?;
            }
        }
        if !st.untracked.is_empty() {
            writeln!(out, "Untracked files:")?;
            for p in &st.untracked {
                writeln!(out, "\t{p}")?;
            }
        }
        if unmerged.is_empty()
            && st.staged.is_empty()
            && st.unstaged.is_empty()
            && st.untracked.is_empty()
        {
            writeln!(out, "nothing to commit, working tree clean")?;
        }
        Ok(())
    }
}

/// Renders `status` as the stable JSON schema (version 1):
/// `{schema_version, branch, principal:{kind,id,session?}, staged:[…],
/// unstaged:[…], untracked:[…], unmerged:[…], clean}`. `change` ∈
/// `added`/`modified`/`deleted`; `principal` is the structured actor, so
/// an agent can self-check "who am I to this repo" via the same
/// machine-first surface it uses for everything else.
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
        ChangeKind::Deleted => "deleted",
    };
    let entries = |v: &[(BString, ChangeKind)]| {
        Json::Array(
            v.iter()
                .map(|(p, k)| {
                    Json::Object(vec![
                        ("path", Json::str(p)),
                        ("change", Json::str(change(*k))),
                    ])
                })
                .collect(),
        )
    };
    let paths = |it: &mut dyn Iterator<Item = &BString>| Json::Array(it.map(Json::str).collect());
    let clean = unmerged.is_empty()
        && st.staged.is_empty()
        && st.unstaged.is_empty()
        && st.untracked.is_empty();
    let doc = Json::Object(vec![
        ("schema_version", Json::Num(1)),
        ("branch", Json::str(branch)),
        ("principal", principal_json(principal)),
        ("staged", entries(&st.staged)),
        ("unstaged", entries(&st.unstaged)),
        ("untracked", paths(&mut st.untracked.iter())),
        ("unmerged", paths(&mut unmerged.iter())),
        ("clean", Json::Bool(clean)),
    ]);
    doc.write(out)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// The mode `text` names, by its name or as a boolean in one of the
/// spellings git takes: true is `normal`, false is `no`.
fn mode_named(text: &[u8]) -> Option<Untracked> {
    match text.to_ascii_lowercase().as_slice() {
        b"no" | b"false" | b"off" | b"0" => Some(Untracked::No),
        b"normal" | b"true" | b"yes" | b"on" | b"1" => Some(Untracked::Normal),
        b"all" => Some(Untracked::All),
        _ => None,
    }
}
