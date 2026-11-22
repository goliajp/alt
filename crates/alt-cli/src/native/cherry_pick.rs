//! `alt cherry-pick`: apply one commit's changes on top of the branch.

use super::*;

mod resume;
mod state;

impl NativeRepo<'_> {
    /// `alt cherry-pick <rev>`: take the changes introduced by commit
    /// `<rev>` (the diff between it and its first parent) and apply
    /// them on top of the current branch, then commit with the
    /// original author + a copy of the message tagged
    /// `(cherry picked from commit <oid>)`. Returns `Ok(true)` when the
    /// 3-way merge stops in conflict (caller maps to exit code 1).
    pub fn cherry_pick(&mut self, rev: &str, json: bool, out: &mut impl Write) -> Res<bool> {
        self.ensure_topic_branch_or_unborn("cherry-pick")?;
        let repo = self.store.git_view()?;
        let target_oid = repo
            .rev_parse(rev)?
            .ok_or_else(|| settings::fatal(format!("bad revision '{rev}'")))?;
        let target_commit = repo.read_commit(&target_oid)?;
        let target_parent = target_commit
            .parents()
            .next()
            .ok_or("cannot cherry-pick a root commit")?;

        let head_ref = self.head_branch()?;
        let head_oid = self
            .store
            .refs
            .resolve(&head_ref)?
            .ok_or("cannot cherry-pick before the first commit")?;
        self.ensure_clean("cherry-pick")?;
        let committer = self.committer()?;

        let base_entries = self.commit_entries(target_parent)?;
        let ours_entries = self.commit_entries(head_oid)?;
        let theirs_entries = self.commit_entries(target_oid)?;
        let label = target_oid.to_string();
        let label_short = &label[..label.len().min(8)];

        let resolved =
            self.merge_trees(&base_entries, &ours_entries, &theirs_entries, label_short)?;
        self.store.odb.flush()?;

        if resolved.iter().any(|r| r.conflicted) {
            self.start_cherry_pick(target_oid, head_oid)?;
            self.write_conflicted(&resolved)?;
            let conflicts: Vec<BString> = resolved
                .iter()
                .filter(|r| r.conflicted)
                .map(|r| r.path.clone())
                .collect();
            if json {
                self.report_merge(true, out, "conflicted", None, &conflicts, "")?;
            } else {
                say_conflicts(out, "cherry-pick", &conflicts)?;
                writeln!(
                    out,
                    "error: could not apply {label_short}; fix the conflicts, `alt add` them \
                     and run `alt cherry-pick --continue` (or `alt cherry-pick --abort`)"
                )?;
            }
            return Ok(true);
        }

        // read through `repo` before the store starts its write batch
        let head_commit = repo.read_commit(&head_oid)?;
        let entries: Vec<WorkEntry> = resolved.iter().filter_map(|r| r.entry.clone()).collect();
        let algo = self.store.algo()?;
        let tree = write_tree(&mut self.store.odb, &entries, algo)?;

        // Did the cherry-pick actually change anything? If the resulting
        // tree matches HEAD's, there's nothing to commit.
        if head_commit.tree() == Some(tree) {
            writeln!(
                out,
                "Already applied; nothing to commit (cherry pick is empty)."
            )?;
            return Ok(false);
        }

        // Apply the new tree to the working tree + index, just like
        // `alt merge`'s success path does.
        let old = index_entries(&self.index()?);

        let (author_sig, msg) = picked_identity(&target_commit, target_oid)?;

        let commit = write_commit(
            &mut self.store.odb,
            tree,
            &[head_oid],
            &author_sig,
            &committer.sig(),
            &msg,
            algo,
        )?;
        self.store.odb.flush()?;
        self.advance_branch_checkout(&head_ref, head_oid, commit, &old, &entries)?;

        report_picked(&target_commit, target_oid, commit, json, out)?;
        Ok(false)
    }
}

/// The author a pick of `source` keeps and the message it is committed
/// with.
fn picked_identity(
    source: &alt_git_codec::Commit,
    picked: ObjectId,
) -> Res<(Sig<'static>, String)> {
    let author = parse_ident(source.author().map(|b| b.to_vec()).unwrap_or_default())?;
    Ok((author, picked_message(source.message().as_slice(), picked)))
}

/// Says that `source`, the commit `picked`, was applied as `commit`.
fn report_picked(
    source: &alt_git_codec::Commit,
    picked: ObjectId,
    commit: ObjectId,
    json: bool,
    out: &mut impl Write,
) -> Res<()> {
    if json {
        use crate::json::Json;
        crate::json::emit(
            out,
            vec![
                ("result", Json::str("applied")),
                ("source", Json::str(picked.to_string())),
                ("commit", Json::str(commit.to_string())),
            ],
        )?;
    } else {
        let message = source.message().as_slice();
        let subject = message.split(|&b| b == b'\n').next().unwrap_or_default();
        writeln!(out, "[{commit}] {}", String::from_utf8_lossy(subject))?;
    }
    Ok(())
}

/// The picked commit's message, ending in the trailer that names it.
fn picked_message(original: &[u8], picked: ObjectId) -> String {
    let mut msg = String::from_utf8_lossy(original).into_owned();
    if !msg.ends_with('\n') {
        msg.push('\n');
    }
    if !msg.ends_with("\n\n") {
        msg.push('\n');
    }
    msg.push_str(&format!("(cherry picked from commit {picked})\n"));
    msg
}

/// Parse a git ident line ("Name <email> <epoch> <tz>") into a
/// [`Sig`] suitable for [`build_commit_bytes`]. Tolerates trailing
/// whitespace; falls back to `unknown / unknown@invalid / 0 / +0000`
/// for any field that won't parse, so a cherry-pick of a weirdly
/// formatted commit still goes through (the author header on the
/// resulting commit may differ from upstream, but the operation
/// succeeds).
fn parse_ident(raw: Vec<u8>) -> Res<Sig<'static>> {
    use std::sync::OnceLock;
    static UNKNOWN_NAME: OnceLock<String> = OnceLock::new();
    static UNKNOWN_EMAIL: OnceLock<String> = OnceLock::new();
    static TZ_PLUS_0000: OnceLock<String> = OnceLock::new();
    let unknown_name: &'static str = UNKNOWN_NAME.get_or_init(|| "unknown".into());
    let unknown_email: &'static str = UNKNOWN_EMAIL.get_or_init(|| "unknown@invalid".into());
    let default_tz: &'static str = TZ_PLUS_0000.get_or_init(|| "+0000".into());

    let s = String::from_utf8_lossy(&raw).into_owned();
    let lt = match s.find('<') {
        Some(p) => p,
        None => {
            return Ok(Sig {
                name: unknown_name,
                email: unknown_email,
                when: 0,
                tz: default_tz,
            });
        }
    };
    let gt_rel = match s[lt..].find('>') {
        Some(p) => p,
        None => {
            return Ok(Sig {
                name: unknown_name,
                email: unknown_email,
                when: 0,
                tz: default_tz,
            });
        }
    };
    let name = s[..lt].trim().to_string();
    let email = s[lt + 1..lt + gt_rel].to_string();
    let rest = s[lt + gt_rel + 1..].trim();
    let mut parts = rest.split_whitespace();
    let when: i64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let tz = parts.next().unwrap_or("+0000").to_string();

    // We need 'static &str refs because Sig borrows them; intern the
    // strings in a small per-call leak. Cherry-picks aren't called in
    // a hot loop, so this is harmless. The signature only lives for
    // the duration of the cherry-pick call.
    Ok(Sig {
        name: Box::leak(name.into_boxed_str()),
        email: Box::leak(email.into_boxed_str()),
        when,
        tz: Box::leak(tz.into_boxed_str()),
    })
}

/// git's line for each of the `conflicts` a `kind` of merge left, the path
/// as its bytes are.
pub(super) fn say_conflicts(out: &mut impl Write, kind: &str, conflicts: &[BString]) -> Res<()> {
    for path in conflicts {
        write!(out, "CONFLICT (content): {kind} conflict in ")?;
        out.write_all(path)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}
