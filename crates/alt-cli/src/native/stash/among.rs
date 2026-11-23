//! The paths a `stash push` is asked to put away.

use super::super::pathspec::{Named, Spec};
use super::super::*;

/// What a push is asked for.
#[derive(Clone, Copy)]
pub struct StashPush<'a> {
    /// the message of the entry
    pub message: Option<&'a str>,
    /// the untracked files are put away as well
    pub untracked: bool,
    /// the paths to put away, all of them where none is given
    pub paths: &'a [String],
}

/// The paths given to a push, read from where the command runs.
pub(super) struct Among {
    specs: Vec<Spec>,
    /// how far into the tree the command runs
    prefix: usize,
}

impl Among {
    /// Whether `path` is one to put away.
    pub(super) fn has(&self, path: &[u8]) -> bool {
        self.specs.is_empty() || Named::of(&self.specs).has(path)
    }

    /// Whether every path is.
    pub(super) fn all(&self) -> bool {
        self.specs.is_empty()
    }

    /// git's refusal of a path given that names none of `tracked`.
    pub(super) fn ensure_known<'p>(
        &self,
        tracked: impl Iterator<Item = &'p BString> + Clone,
    ) -> Res<()> {
        for spec in self.specs.iter().filter(|spec| !spec.exclude) {
            if tracked.clone().any(|path| spec.matches(path)) {
                continue;
            }
            // git says how much of the path is where the command runs,
            // which is none of it once the path climbs out of there
            let climbs = spec.given.split('/').any(|step| step == "..");
            let prefix = if climbs { 0 } else { self.prefix };
            return Err(said::refused(
                &format!(
                    "error: pathspec ':(prefix:{prefix}){}' did not match any file(s) known \
                     to git\nDid you forget to 'git add'?\n",
                    spec.path
                ),
                128,
            ));
        }
        Ok(())
    }
}

impl NativeRepo<'_> {
    /// The paths `given` to a push.
    pub(super) fn among(&self, given: &[String]) -> Res<Among> {
        let parse = |given: &String| Spec::parse(&self.root, &self.prefix, given);
        Ok(Among {
            specs: given.iter().map(parse).collect::<Res<_>>()?,
            // git counts the slash that ends the directory
            prefix: match self.prefix.len() {
                0 => 0,
                len => len + 1,
            },
        })
    }
}
