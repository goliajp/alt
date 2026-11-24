//! Revisions that ask the log of a ref: `<ref>@{N}` and `@{-N}`.

use alt_git_codec::ObjectId;

use crate::{RepoError, Repository};

impl Repository {
    /// The full name of the ref `named` is found by: a branch left
    /// before for `@{-N}`, else the first of git's lookup order there is.
    pub fn full_name(&self, named: &str) -> Result<Option<String>, RepoError> {
        if let Some((head, nth)) = named.strip_suffix('}').and_then(|n| n.rsplit_once("@{-")) {
            // as git: `@{-N}` stands after no name, `HEAD` no more than another
            if head == "HEAD" {
                return Ok(None);
            }
            let head = if head.is_empty() { "HEAD" } else { head };
            let Some(nth) = nth.parse::<usize>().ok().and_then(|nth| nth.checked_sub(1)) else {
                return Ok(None);
            };
            let left = self.branch_before(head, nth)?;
            return Ok(left.map(|short| format!("refs/heads/{short}")));
        }
        for candidate in [
            named.to_owned(),
            format!("refs/{named}"),
            format!("refs/tags/{named}"),
            format!("refs/heads/{named}"),
            format!("refs/remotes/{named}"),
        ] {
            if self.resolve_ref(&candidate)?.is_some() {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// `<named>@{<asked>}`, written `spec`, where `asked` is of the log:
    /// `N`, `-N`, or `.N` for the branch the head ref `named` is on, which
    /// is how `@{N}` alone is spelled for a workspace.
    pub(crate) fn logged(
        &self,
        spec: &str,
        named: &str,
        asked: &str,
    ) -> Result<Option<ObjectId>, RepoError> {
        let number = |digits: &str| {
            let plain = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
            plain.then(|| digits.parse::<usize>().ok()).flatten()
        };
        if let Some(nth) = asked.strip_prefix('-') {
            if named == "HEAD" {
                return Ok(None);
            }
            let head = if named.is_empty() { "HEAD" } else { named };
            let Some(nth) = number(nth).and_then(|nth| nth.checked_sub(1)) else {
                return Ok(None);
            };
            let Some(left) = self.branch_before(head, nth)? else {
                return Ok(None);
            };
            if let Some(branch) = self.resolve_ref(&format!("refs/heads/{left}"))? {
                return Ok(Some(branch));
            }
            // git logs the commit that was left where it was on no branch
            return match left.contains("@{") {
                true => Ok(None),
                false => self.resolve_base(&left),
            };
        }
        let (own, digits) = match asked.strip_prefix('.') {
            Some(digits) => (true, digits),
            None => (named.is_empty(), asked),
        };
        let Some(back) = number(digits) else {
            return Err(RepoError::LogByDate(spec.to_owned()));
        };
        let (name, shown) = if own {
            let head = if named.is_empty() { "HEAD" } else { named };
            let Some(branch) = self.branch_of(head)? else {
                return Ok(None);
            };
            (format!("refs/heads/{branch}"), branch.to_string())
        } else {
            let Some(name) = self.full_name(named)? else {
                return Ok(None);
            };
            let head =
                name == "HEAD" || (name.starts_with("workspaces/") && name.ends_with("/HEAD"));
            let shown = if head { "HEAD" } else { named };
            (name, shown.to_owned())
        };
        if back == 0 {
            return self.resolve_ref(&name);
        }
        match self.logged_at(&name, back)? {
            Ok(oid) => Ok(Some(oid)),
            Err(0) => Ok(None),
            Err(has) => Err(RepoError::LogShort { name: shown, has }),
        }
    }
}
