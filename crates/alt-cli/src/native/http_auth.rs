//! Credentials for the HTTP transports (git smart HTTP, LFS).

/// Build an [`alt_wire_http::GitTransport`] for `url`. Credential
/// resolution order:
///
/// 1. Env vars: `ALT_HTTP_USER_<NAME>` + `ALT_HTTP_TOKEN_<NAME>` —
///    explicit, scripted runs. Always win.
/// 2. `git credential fill` — when alt finds a usable `git` on PATH it
///    asks it to resolve the URL, then forwards whatever username /
///    password the configured helper (osxkeychain, manager, store, …)
///    returns. This is what makes alt push to GitHub Just Work for a
///    user who's already done `gh auth login` or `git push` once
///    before.
/// 3. Nothing — anonymous request. Public repos work, private repos
///    get HTTP 401 and a clear pointer at the auth options.
pub(super) fn build_transport(remote_name: &str, url: &str) -> alt_wire_http::GitTransport {
    let t = alt_wire_http::GitTransport::new(url);
    match http_auth(remote_name, url) {
        Some(auth) => t.with_auth(auth),
        None => t,
    }
}

/// The credentials for `url` under remote `remote_name`, in the order
/// [`build_transport`] documents.
pub(super) fn http_auth(remote_name: &str, url: &str) -> Option<alt_wire_http::BasicAuth> {
    let env_key = remote_name
        .chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect::<String>();
    let user = std::env::var(format!("ALT_HTTP_USER_{env_key}")).ok();
    let token = std::env::var(format!("ALT_HTTP_TOKEN_{env_key}")).ok();
    if let (Some(username), Some(token)) = (user, token) {
        return Some(alt_wire_http::BasicAuth { username, token });
    }
    git_credential_fill(url)
}

/// Ask `git credential fill` to resolve auth for `url`. Returns
/// `None` when:
/// - `git` isn't on PATH
/// - the URL can't be split into a protocol + host (alt-wire's
///   transport already rejects malformed URLs further down)
/// - `git credential` exits non-zero (no helper configured, helper
///   declined, …) — anonymous request is fine, public repos work
///
/// Successful invocations carry username/password back over stdout in
/// the canonical key=value form; we forward them to alt-wire as Basic
/// auth. The opt-out is `ALT_NO_CREDENTIAL_HELPER=1` for users who
/// prefer to fail fast on misconfigured helpers.
fn git_credential_fill(url: &str) -> Option<alt_wire_http::BasicAuth> {
    if std::env::var_os("ALT_NO_CREDENTIAL_HELPER").is_some() {
        return None;
    }

    // Split scheme://host[:port]/...
    let scheme_split = url.find("://")?;
    let scheme = &url[..scheme_split];
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let after_scheme = &url[scheme_split + 3..];
    let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let host = &after_scheme[..host_end];
    let path_part = if host_end < after_scheme.len() {
        &after_scheme[host_end..]
    } else {
        "/"
    };

    // Prepare stdin: `protocol=…\nhost=…\npath=…\n\n`
    let stdin_body = format!(
        "protocol={scheme}\nhost={host}\npath={path}\n\n",
        path = path_part.trim_start_matches('/'),
    );

    let mut child = std::process::Command::new("git")
        .args(["credential", "fill"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take()?;
        stdin.write_all(stdin_body.as_bytes()).ok()?;
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let body = std::str::from_utf8(&output.stdout).ok()?;
    let mut username: Option<String> = None;
    let mut password: Option<String> = None;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("username=") {
            username = Some(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix("password=") {
            password = Some(rest.to_owned());
        }
    }
    let username = username.unwrap_or_default();
    let password = password?;
    Some(alt_wire_http::BasicAuth {
        username,
        token: password,
    })
}
