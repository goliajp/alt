//! The lock of a workspace.

use std::fs::File;

use super::*;

/// Takes the one lock that commands which write in the workspace whose
/// index is at `index_path` hold, and waits for it.
pub(super) fn locked(index_path: &Path) -> Res<File> {
    let path = index_path.with_file_name("write.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    #[cfg(unix)]
    {
        loop {
            match alt_os::sys::flock(&file, alt_os::sys::LOCK_EX) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    #[cfg(not(unix))]
    return Err("workspace mutations require a supported Unix filesystem".into());
    Ok(file)
}

impl NativeRepo<'_> {
    /// One OS lock per workspace, retained for this view's lifetime. Separate
    /// workspaces continue independently; shared refs still use their CAS lock.
    /// Under it, what a command cut short left in the tree of a parallel
    /// workspace to ask the file system in is cleared away.
    pub(in crate::native) fn lock_workspace(&mut self) -> Res<()> {
        if self.write_lock.is_none() {
            self.write_lock = Some(locked(&self.index_path)?);
            self.store.refresh()?;
            self.caps = self.store.capabilities_for(&self.id.principal);
            if self.workspace != DEFAULT_WORKSPACE {
                checkout_files::clear_left(&self.root)?;
            }
        }
        Ok(())
    }
}

impl Store {
    /// The lock of the workspace `named` from `cwd`, for a removal of
    /// lost records `id` asks for: no command changes the index of that
    /// workspace between the listing of what names the lost objects and
    /// the removal. The store is not refreshed under the lock and no
    /// operation left open is looked for, as they are for other commands
    /// that write: a removal is asked for where a refresh that writes
    /// refuses, and where an operation cannot be recovered before it.
    pub(crate) fn hold_workspace(
        &self,
        cwd: &Path,
        named: Option<&str>,
        id: &Identity,
    ) -> Res<File> {
        if self.capabilities_for(&id.principal).read_only {
            return Err(crate::machine::Failure::refusal(
                "capability_denied",
                "capability denied: principal cannot write (read-only)",
            )
            .into());
        }
        let (_, coord) = resolve_workspace(cwd, named)?;
        locked(&coord.index_path)
    }
}
