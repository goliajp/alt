//! A repository of its own in the working tree, named to `add`.

use alt_worktree::Delta;

use super::*;

impl NativeRepo<'_> {
    /// The scan reports a repository of its own in a directory the index
    /// knows nothing in as that directory, with `/` at the end. Each of
    /// them among the named paths is staged as git stages it: as a
    /// submodule at the commit it has checked out, and it is an error when
    /// it has none. The others are left where they are. Returns the paths
    /// taken.
    pub(super) fn take_embedded(
        &self,
        delta: &mut Delta,
        named: &dyn Fn(&[u8]) -> bool,
    ) -> Res<Vec<BString>> {
        let mut taken = Vec::new();
        if !delta.untracked.iter().any(|path| path.ends_with(b"/")) {
            return Ok(taken);
        }
        let attrs = self.converter()?;
        for dir in std::mem::take(&mut delta.untracked) {
            let Some(path) = dir.strip_suffix(b"/") else {
                delta.untracked.push(dir);
                continue;
            };
            if !named(path) {
                continue;
            }
            let path = BString::from(path);
            let head = attrs.submodule_head(&self.abs(&path)?);
            attrs.check()?;
            let oid = head.ok_or_else(|| format!("'{dir}' does not have a commit checked out"))?;
            delta.changed.push(WorkEntry {
                path: path.clone(),
                oid,
                mode: 0o160000,
            });
            taken.push(path);
        }
        delta.changed.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(taken)
    }
}
