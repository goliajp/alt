//! SSH-style remote URLs of well-known hosts, rewritten to HTTPS.

/// Built-in fallback for SSH-style URLs against well-known git
/// hosting endpoints whose HTTPS path mirrors the SSH path.
/// Recognises both forms git accepts:
///   git@github.com:user/repo.git
///   ssh://git@github.com/user/repo.git
/// → https://github.com/user/repo.git
///
/// Returns `None` for anything that isn't on the allow-list — keeps
/// the surface predictable; users with non-listed hosts can configure
/// `[url "X"] insteadOf = Y` explicitly.
pub(crate) fn builtin_ssh_to_https(url: &str) -> Option<String> {
    const HOSTS: &[&str] = &[
        "github.com",
        "gitlab.com",
        "bitbucket.org",
        "codeberg.org",
        "git.sr.ht",
    ];
    for host in HOSTS {
        // `git@host:path`
        let scp_prefix = format!("git@{host}:");
        if let Some(path) = url.strip_prefix(&scp_prefix) {
            return Some(format!("https://{host}/{path}"));
        }
        // `ssh://git@host/path`
        let ssh_prefix = format!("ssh://git@{host}/");
        if let Some(path) = url.strip_prefix(&ssh_prefix) {
            return Some(format!("https://{host}/{path}"));
        }
    }
    None
}
