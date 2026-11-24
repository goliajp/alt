//! The requests of a fetch: the advertisement, `ls-refs` and `fetch`.

use super::*;

/// preflight + bare OPTIONS responder.
///
/// Returns 204 + `Allow:` listing the methods the matched endpoint
/// supports (or `GET, POST, OPTIONS` when the OPTIONS hit doesn't
/// resolve to a known endpoint — closer to "what the server speaks"
/// than spec-strict but useful as a probe response).
///
/// CORS: opt-in via `ALT_SERVER_CORS_ALLOW_ORIGIN`. Defaults to no
/// `Access-Control-*` headers, which keeps a default-config server
/// off the open-CORS attack surface. Operators who want web UI
/// access set the env to the exact origin (`https://ui.example.com`)
/// or `*` for fully-open dev mode.
pub(super) fn build_options_response(
    endpoint_allow: Option<&'static str>,
) -> Response<Cursor<Vec<u8>>> {
    let allow = endpoint_allow.unwrap_or("GET, POST, OPTIONS");
    let mut resp = Response::from_data(Vec::<u8>::new()).with_status_code(StatusCode(204));
    resp.add_header(header("Allow", allow));
    if let Ok(origin) = std::env::var("ALT_SERVER_CORS_ALLOW_ORIGIN")
        && !origin.is_empty()
    {
        resp.add_header(header("Access-Control-Allow-Origin", &origin));
        resp.add_header(header("Access-Control-Allow-Methods", allow));
        resp.add_header(header(
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization, Git-Protocol, Content-Encoding",
        ));
        resp.add_header(header("Access-Control-Max-Age", "86400"));
    }
    resp
}

pub(super) fn parse_query<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    for chunk in query.split('&') {
        if let Some((k, v)) = chunk.split_once('=')
            && k == key
        {
            return Some(v);
        }
    }
    None
}

pub(super) fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static header literals are valid")
}

/// POST /git-upload-pack. v2 dispatch on the first `command=…` line:
/// `ls-refs` and `fetch` both land here. The request body
/// header is identical (`command=<x>\n` + optional `object-format=…`),
/// only the args section differs — sniff the first frame to route.
pub(super) fn handle_upload_pack(
    repo: &Repository,
    mut req: http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut body = Vec::new();
    if let Err(e) = read_body_capped(&mut req, &mut body, max_command_bytes()) {
        let r = Response::from_string(format!("{e}")).with_status_code(limits::refusal(&*e));
        respond_logged(req, r, log)?;
        return Ok(());
    }
    let cmd = sniff_command(&body)?;

    let mut out = Vec::new();
    match cmd.as_str() {
        "ls-refs" => {
            let (lsr_req, _fmt) =
                alt_wire::ls_refs::parse_ls_refs_request(&mut Cursor::new(&body))?;
            let refs = read_refs(repo, &lsr_req)?;
            alt_wire::ls_refs::encode_ls_refs_response(&mut out, &refs)?;
        }
        "fetch" => {
            let fetch_req = match alt_wire::fetch::parse_fetch_request(&mut Cursor::new(&body)) {
                Ok((fetch_req, _fmt)) => fetch_req,
                Err(e) => {
                    let r = Response::from_string(e.to_string()).with_status_code(StatusCode(400));
                    return respond_logged(req, r, log).map_err(Into::into);
                }
            };
            let (room, from) = (sending::Room::shared(), req.from());
            let sent = match fetch::response(repo, &fetch_req, room, from) {
                Ok(sent) => sent,
                Err(e) if limits::refusal(&*e) == 503 => {
                    let mut r = Response::from_string(format!("{e}")).with_status_code(503);
                    r.add_header(header("Retry-After", "60"));
                    return respond_logged(req, r, log).map_err(Into::into);
                }
                Err(e) => return Err(e),
            };
            let mut resp = Response::sized(sent.length(), sent);
            resp.add_header(header("Content-Type", UPLOAD_PACK_RESULT));
            resp.add_header(header("Cache-Control", "no-cache"));
            return Ok(respond_logged(req, resp, log)?);
        }
        other => {
            let r = Response::from_string(format!("unknown command={other}"))
                .with_status_code(StatusCode(400));
            respond_logged(req, r, log)?;
            return Ok(());
        }
    }

    let mut resp = Response::from_data(out);
    resp.add_header(header("Content-Type", UPLOAD_PACK_RESULT));
    resp.add_header(header("Cache-Control", "no-cache"));
    respond_logged(req, resp, log)?;
    Ok(())
}

/// Find the leading `command=<name>` line of a v2 request body so the
/// dispatch can pick its parser. The body is a stream of pkt-lines; the
/// first data frame after the optional `# service=…` is the command.
pub(super) fn sniff_command(body: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    let mut r = Cursor::new(body);
    let mut scratch = Vec::new();
    loop {
        match alt_wire::pkt::read_frame(&mut r, &mut scratch)? {
            alt_wire::pkt::Frame::Data(d) => {
                let trimmed = trim_newline(d);
                let s = std::str::from_utf8(trimmed)?;
                if let Some(name) = s.strip_prefix("command=") {
                    return Ok(name.to_owned());
                }
                // skip non-command headers
            }
            alt_wire::pkt::Frame::Delim
            | alt_wire::pkt::Frame::Flush
            | alt_wire::pkt::Frame::ResponseEnd => {
                return Err("v2 request missing command= header".into());
            }
        }
    }
}

pub(super) fn trim_newline(b: &[u8]) -> &[u8] {
    let mut end = b.len();
    while end > 0 && (b[end - 1] == b'\n' || b[end - 1] == b'\r') {
        end -= 1;
    }
    &b[..end]
}

pub(super) fn handle_info_refs(
    repo: &Repository,
    writer: Option<&WriteCoordinator>,
    query: &str,
    req: http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    let service = parse_query(query, "service")
        .ok_or("info/refs missing ?service= query parameter")?
        .to_owned();
    let body = match service.as_str() {
        // Fetch (read): v2 capability advert. Client posts ls-refs / fetch
        // next over POST /git-upload-pack.
        "git-upload-pack" => {
            let mut body = Vec::new();
            caps::encode_capability_advertisement(
                &mut body,
                &service,
                AGENT,
                Some("sha1"),
                &[
                    ("ls-refs", Some("unborn")),
                    // advertise `filter` so git's partial-clone
                    // path (`--filter=blob:none` etc) negotiates against us
                    ("fetch", Some("shallow wait-for-done filter")),
                ],
            )?;
            body
        }
        // Push (write): receive-pack is still v0/v1 in git. The advert
        // carries the actual ref list inline so the client can compute
        // what to push.
        "git-receive-pack" => {
            let mut body = Vec::new();
            let refs: Vec<(String, ObjectId)> = repo
                .list_refs()?
                .into_iter()
                .filter(|(name, _, _)| name != "HEAD")
                .map(|(name, oid, _)| (name, oid))
                .collect();
            // issue a single-use nonce per advert and attach
            // it as the `alt-nonce=<hex>` capability. A signing alt
            // client will sign `nonce <hex>\n` + canonical payload and
            // echo the same `alt-nonce=<hex>` capability back on its
            // push, so the server can look the nonce up, verify the
            // signature, and consume it. Anyone replaying the same
            // captured push body gets refused on the second attempt.
            //
            // Read-only mode (no writer) doesn't issue nonces; pushes
            // are refused before they ever get verified anyway.
            let nonce_cap: Option<String> = writer.map(|w| {
                let n = w.nonces.issue();
                format!("{}={n}", alt_wire::CAP_ALT_NONCE)
            });
            let mut caps_list: Vec<&str> = vec![
                "report-status",
                "delete-refs",
                "ofs-delta",
                "side-band-64k",
                concat!("agent=", "alt-server/", env!("CARGO_PKG_VERSION")),
            ];
            if let Some(s) = nonce_cap.as_deref() {
                caps_list.push(s);
            }
            alt_wire::push::encode_v1_ref_advertisement(
                &mut body,
                &refs,
                &caps_list,
                HashAlgo::Sha1,
            )?;
            body
        }
        _ => {
            let r = Response::from_string("unknown service").with_status_code(StatusCode(400));
            respond_logged(req, r, log)?;
            return Ok(());
        }
    };
    let content_type = format!("application/x-{service}-advertisement");
    let mut resp = Response::from_data(body);
    resp.add_header(header("Content-Type", &content_type));
    resp.add_header(header("Cache-Control", "no-cache"));
    respond_logged(req, resp, log)?;
    Ok(())
}

/// Read the repo's refs into the ls-refs `RefRecord` shape the wire
/// encoder consumes. Used by the POST /git-upload-pack handler.
pub(super) fn read_refs(
    repo: &Repository,
    req: &alt_wire::ls_refs::LsRefsRequest,
) -> Result<Vec<RefRecord>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    let prefixes = req.prefixes();
    for (name, oid, sym) in repo.list_refs()? {
        if !alt_wire::ls_refs::LsRefsRequest::listed(&prefixes, &name) {
            continue;
        }
        let symref_target = if req.symrefs { sym } else { None };
        out.push(RefRecord {
            oid,
            name,
            symref_target,
            peeled: None,
            other: Default::default(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}
