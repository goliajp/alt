//! `core.precomposeUnicode`: what a command is given is taken in its
//! composed form, as the names read from directories are. A name typed
//! or completed from a listing may be decomposed, and names no path of
//! the index then.

use std::ffi::OsString;

/// `args` as the command reads them: on macOS, where the repository the
/// command runs in asks for it, each in its composed form.
pub fn argv(args: Vec<OsString>) -> Vec<OsString> {
    let text = |arg: &OsString| arg.to_str().is_some_and(|arg| !arg.is_ascii());
    if !cfg!(target_os = "macos") || !args.iter().any(text) || !asked() {
        return args;
    }
    let composed = |arg: OsString| {
        let bytes = alt_worktree::composed(arg.as_encoded_bytes());
        match bytes.map(String::from_utf8) {
            Some(Ok(composed)) => OsString::from(composed),
            _ => arg,
        }
    };
    args.into_iter().map(composed).collect()
}

/// Whether the repository of the working directory sets the variable.
fn asked() -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let Ok((alt_dir, _)) = crate::native::resolve_workspace(&cwd, None) else {
        return false;
    };
    let settings = crate::git_settings::applying(&alt_dir.join("git-import"), || None);
    settings.is_ok_and(|read| crate::git_settings::precompose(&read.config).unwrap_or(false))
}
