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
            if !named(&dir) {
                continue;
            }
            let path = BString::from(path);
            let head = attrs.submodule_head(&self.abs(&path)?);
            attrs.check()?;
            let Some(oid) = head else {
                let why = [
                    b"error: '",
                    &dir[..],
                    b"' does not have a commit checked out\n",
                ];
                crate::diagnostics::emit(&why.concat())?;
                return Err(settings::fatal("adding files failed".into()));
            };
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

/// What `add` says of the repositories at `paths` that it staged as
/// submodules: git's warning for each, and once what it means, unless
/// `advice.addEmbeddedRepo` is off; a path is said as its bytes are. Git names a command that takes the
/// one path out of the index, which this program has none for.
pub(super) fn warning(paths: &[BString], settings: &alt_git_config::Config) -> Res<Vec<u8>> {
    let mut said = Vec::new();
    let advise = match settings.get_bool("advice", None, "addembeddedrepo") {
        Some(set) => set?,
        None => true,
    };
    for (nth, path) in paths.iter().enumerate() {
        said.extend_from_slice(b"warning: adding embedded git repository: ");
        said.extend_from_slice(path);
        said.push(b'\n');
        if nth == 0 && advise {
            said.extend_from_slice(ADVICE.as_bytes());
        }
    }
    Ok(said)
}

const ADVICE: &str = "\
hint: You've added another git repository inside your current repository.
hint: Clones of the outer repository will not contain the contents of
hint: the embedded repository and will not know how to obtain it.
hint:
hint: If you added this path by mistake, you can take back the `alt add`
hint: that staged it, which takes back every path it staged, with:
hint:
hint: \talt undo
hint:
hint: Disable this message with \"alt config advice.addEmbeddedRepo false\"
";
