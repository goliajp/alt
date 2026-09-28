//! `alt cherry-pick`: apply one commit's changes on top of the branch.

use super::*;

impl NativeRepo<'_> {
    /// `alt cherry-pick <rev>`: take the changes introduced by commit
    /// `<rev>` (the diff between it and its first parent) and apply
    /// them on top of the current branch, then commit with the
    /// original author + a copy of the message tagged
    /// `(cherry picked from commit <oid>)`. Returns `Ok(true)` when the
    /// 3-way merge stops in conflict (caller maps to exit code 1).
    pub fn cherry_pick(&mut self, rev: &str, json: bool, out: &mut impl Write) -> Res<bool> {
        self.ensure_topic_branch_or_unborn("cherry-pick")?;
        let repo = alt_repo::Repository::discover(&self.store.alt_dir)?;
        let target_oid = repo
            .rev_parse(rev)?
            .ok_or_else(|| format!("bad revision '{rev}'"))?;
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

        let base_entries = self.commit_entries(target_parent)?;
        let ours_entries = self.commit_entries(head_oid)?;
        let theirs_entries = self.commit_entries(target_oid)?;
        let label = target_oid.to_string();
        let label_short = &label[..label.len().min(8)];

        let resolved =
            self.merge_trees(&base_entries, &ours_entries, &theirs_entries, label_short)?;
        self.store.odb.flush()?;

        if resolved.iter().any(|r| r.conflicted) {
            self.write_conflicted(&resolved)?;
            let conflicts: Vec<BString> = resolved
                .iter()
                .filter(|r| r.conflicted)
                .map(|r| r.path.clone())
                .collect();
            if json {
                self.report_merge(true, out, "conflicted", None, &conflicts, "")?;
            } else {
                for p in &conflicts {
                    writeln!(out, "CONFLICT (content): cherry-pick conflict in {p}")?;
                }
                writeln!(
                    out,
                    "Automatic cherry-pick failed; fix conflicts and then commit the result."
                )?;
            }
            return Ok(true);
        }

        let entries: Vec<WorkEntry> = resolved.iter().filter_map(|r| r.entry.clone()).collect();
        let tree = write_tree(&mut self.store.odb, &entries, self.store.algo)?;

        // Did the cherry-pick actually change anything? If the resulting
        // tree matches HEAD's, there's nothing to commit.
        let head_commit = repo.read_commit(&head_oid)?;
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
        self.checkout(&old, &entries)?;

        // Build the commit: keep the original author ident, but
        // committer + commit time are our own identity + now. Message
        // is the target's plus a "(cherry picked from commit …)" trailer.
        let author_sig = parse_ident(
            target_commit
                .author()
                .map(|b| b.to_vec())
                .unwrap_or_default(),
        )?;
        let when = (now_ms() / 1000) as i64;
        let (committer_name, committer_email) = self.id.sig();
        let committer = Sig {
            name: committer_name,
            email: committer_email,
            when,
            tz: "+0000",
        };
        let original_msg = String::from_utf8_lossy(target_commit.message().as_slice()).into_owned();
        let mut msg = original_msg;
        if !msg.ends_with('\n') {
            msg.push('\n');
        }
        if !msg.ends_with("\n\n") {
            msg.push('\n');
        }
        msg.push_str(&format!("(cherry picked from commit {target_oid})\n"));

        let commit = write_commit(
            &mut self.store.odb,
            tree,
            &[head_oid],
            &author_sig,
            &committer,
            &msg,
            self.store.algo,
        )?;
        self.store.odb.flush()?;
        self.advance_branch(&head_ref, head_oid, commit)?;

        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("result", Json::str("applied")),
                    ("source", Json::str(target_oid.to_string())),
                    ("commit", Json::str(commit.to_string())),
                ],
            )?;
        } else {
            let subject = target_commit
                .message()
                .as_slice()
                .split(|&b| b == b'\n')
                .next()
                .map(String::from_utf8_lossy)
                .unwrap_or_default()
                .into_owned();
            writeln!(out, "[{commit}] {subject}")?;
        }
        Ok(false)
    }
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
