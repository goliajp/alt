//! `alt remote list`.

use super::*;

impl NativeRepo<'_> {
    /// `alt remote list`: emit configured remotes (alphabetical). Empty
    /// when no `remotes/` dir exists — a fresh repo has no remotes.
    pub fn remote_list(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let remotes = self.read_remotes()?;
        if json {
            use crate::json::Json;
            let arr = remotes
                .iter()
                .map(|r| {
                    Json::Object(vec![
                        ("name", Json::str(&r.name)),
                        ("url", Json::str(&r.url)),
                        ("fetch", Json::str(&r.fetch)),
                    ])
                })
                .collect();
            crate::json::emit(out, vec![("remotes", Json::Array(arr))])?;
        } else {
            for r in &remotes {
                writeln!(out, "{}\t{}", r.name, r.url)?;
            }
        }
        Ok(())
    }
}

/// The branch a fresh clone checks out, with its commit.
pub(super) fn pick_clone_branch(repo: &NativeRepo) -> Res<Option<(String, ObjectId)>> {
    // (1) `refs/remotes/origin/HEAD` symref written by our fetch path
    if let Some(RefTarget::Symbolic(target)) = repo.store_refs_get("refs/remotes/origin/HEAD")
        && let Some(branch) = target.strip_prefix("refs/remotes/origin/")
        && let Some(RefTarget::Oid(oid)) = repo.store_refs_get(&target)
    {
        return Ok(Some((branch.to_owned(), oid)));
    }
    // (2) common defaults
    for cand in ["main", "master"] {
        let r = format!("refs/remotes/origin/{cand}");
        if let Some(RefTarget::Oid(oid)) = repo.store_refs_get(&r) {
            return Ok(Some((cand.to_owned(), oid)));
        }
    }
    // (3) first remote-tracking branch in name order
    for (name, target) in repo.store_refs_iter() {
        if let Some(branch) = name.strip_prefix("refs/remotes/origin/")
            && branch != "HEAD"
            && let RefTarget::Oid(oid) = target
        {
            return Ok(Some((branch.to_owned(), oid)));
        }
    }
    Ok(None)
}
