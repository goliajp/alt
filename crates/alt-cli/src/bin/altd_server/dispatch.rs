//! One request, from its head to its handler and the line of the access log.

use super::*;

/// One request through dispatch + access-log emission. Pulled out of
/// the main loop so each worker thread runs the same path with no
/// shared mutable state across workers (everything per-request is
/// stack-local).
pub(super) fn serve_one(mode: &ServeMode, req: http::Request) {
    let url = req.url().to_owned();
    let method = req.method().clone();
    let req_id = next_request_id();
    let start = std::time::Instant::now();
    let mut log = LogCtx {
        bytes_in: req.body_length().map(|n| n as u64).unwrap_or(0),
        ..LogCtx::default()
    };
    if let Err(e) = dispatch(mode, method.clone(), &url, req, &mut log) {
        eprintln!("altd-server: handler error: {e} (req_id={req_id})");
        if log.status == 0 {
            log.status = 500;
        }
    }
    if log.deferred {
        return;
    }
    let duration_ms = start.elapsed().as_millis();
    emit_access_log(&req_id, &method, &url, &log, duration_ms);
}

fn dispatch(
    mode: &ServeMode,
    method: Method,
    url: &str,
    req: http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    // optional Basic auth in multi-repo mode. When the server
    // root has a `users` file, every request must carry a valid
    // Authorization header; absence / mismatch returns HTTP 401 with a
    // WWW-Authenticate prompt so a real git client retries with creds.
    // a scoped user (3-column line) hands back an ACL the
    // dispatcher checks against the resolved repo + action below.
    let mut auth_user: Option<String> = None;
    let mut scoped_acl: Option<Vec<AclRule>> = None;
    if let ServeMode::Multi { root, users, .. } = mode {
        let users_path = root.join("users");
        if *users || users_path.is_file() {
            match check_auth(&req, &users_path) {
                AuthOutcome::Allow { user, acl } => {
                    auth_user = user;
                    scoped_acl = acl;
                }
                AuthOutcome::Reject(reason) => {
                    let mut resp = Response::from_string(reason).with_status_code(StatusCode(401));
                    resp.add_header(header("WWW-Authenticate", "Basic realm=\"altd-server\""));
                    respond_logged(req, resp, log)?;
                    return Ok(());
                }
                AuthOutcome::Unread => {
                    let unread = "the users file of the server does not read";
                    let resp = Response::from_string(unread).with_status_code(StatusCode(500));
                    respond_logged(req, resp, log)?;
                    return Ok(());
                }
            }
        }
    }
    log.principal = Some(auth_user.clone().unwrap_or_else(|| "anonymous".to_owned()));

    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    };
    // Multi-repo mode peels the first path segment as the repo name and
    // hands back the remaining suffix; single-repo mode keeps the path
    // intact. After resolve_repo, the suffix always ends in one of the
    // smart-http endpoints if the URL was well-formed.
    let (handle, suffix, repo_name) = match resolve_repo(mode, path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("altd-server: {e}");
            let r = Response::from_string("repository not found").with_status_code(StatusCode(404));
            respond_logged(req, r, log)?;
            return Ok(());
        }
    };
    log.repo = Some(repo_name.clone());

    // gate the resolved request against the scoped user's
    // ACL. Trusted (no-ACL) users skip the check entirely; the request
    // proceeds unscoped.
    if let Some(acl) = &scoped_acl
        && let Some(action) = action_from_request(&method, &suffix, query)
        && !acl_allows(acl, &repo_name, action)
    {
        let r = Response::from_string(format!(
            "forbidden: no {action:?} permission on repo '{repo_name}'"
        ))
        .with_status_code(StatusCode(403));
        respond_logged(req, r, log)?;
        return Ok(());
    }

    // fail-fast 413 when the client *advertised* a body
    // larger than the cap. Lying Content-Length headers are caught
    // at read time inside the handlers via `read_body_capped`.
    if method == Method::Post
        && let Some(advertised) = req.body_length()
        && advertised as u64 > max_body_bytes()
    {
        let max = max_body_bytes();
        let r = Response::from_string(format!(
            "payload too large: Content-Length={advertised} exceeds cap {max}"
        ))
        .with_status_code(StatusCode(413));
        respond_logged(req, r, log)?;
        return Ok(());
    }

    // figure out which (allowed methods, endpoint)
    // the suffix matches first, then check method. A path match with
    // the wrong method returns 405 + Allow header; a real OPTIONS
    // request returns 204 + Allow + (optionally) CORS preflight
    // headers. This replaces the previous blanket-404 path that ate
    // both signals.
    let endpoint_allow: Option<&'static str> = if suffix.ends_with("/info/refs") {
        Some("GET, OPTIONS")
    } else if suffix.ends_with("/git-upload-pack") || suffix.ends_with("/git-receive-pack") {
        Some("POST, OPTIONS")
    } else {
        None
    };

    if method == Method::Options {
        let resp = build_options_response(endpoint_allow);
        respond_logged(req, resp, log)?;
        return Ok(());
    }

    if let Some(allow) = endpoint_allow {
        // a body is received by the thread of its connection, and the
        // request comes here again with it
        if method == Method::Post && !req.received() {
            log.deferred = true;
            req.receive(limits::body_cap(&suffix));
            return Ok(());
        }
        if method == Method::Get && suffix.ends_with("/info/refs") {
            let repo = handle.open_repo()?;
            return handle_info_refs(handle.writer.as_deref(), query, req, log, repo);
        }
        if method == Method::Post && suffix.ends_with("/git-upload-pack") {
            let repo = handle.open_repo()?;
            return handle_upload_pack(req, log, repo, auth_user.is_some());
        }
        if method == Method::Post && suffix.ends_with("/git-receive-pack") {
            let repo = handle.open_repo()?;
            let Some(writer) = handle.writer else {
                let r = Response::from_string("repo is read-only (no .alt write store)")
                    .with_status_code(StatusCode(403));
                return Ok(repo.answer(req, r, log)?);
            };
            let answered = handle_receive_pack(&writer, auth_user.as_deref(), req, log, repo);
            // a sync that failed is known of the repository from here on,
            // though this request be the last to have its writer in hand
            writer.remember();
            return answered;
        }
        // path matched but method didn't — 405 Method Not Allowed
        let mut r = Response::from_string(format!("method not allowed; allow={allow}"))
            .with_status_code(StatusCode(405));
        r.add_header(header("Allow", allow));
        respond_logged(req, r, log)?;
        return Ok(());
    }

    let r = Response::from_string("not found").with_status_code(StatusCode(404));
    respond_logged(req, r, log)?;
    Ok(())
}
