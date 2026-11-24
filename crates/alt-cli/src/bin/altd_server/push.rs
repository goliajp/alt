//! A push: its body read, its signature looked at, its report written.

use super::*;

/// Reads the head of a push off the connection and streams the pack
/// behind it into the file at `pack`, so that only the head is ever in
/// memory, and a push that is refused for its signature never has its
/// pack looked at. One byte past `max` is read at most: the caller tells
/// a body over the cap by the count. The reader's borrow of `req` ends
/// here, before the response is sent.
pub(super) fn read_push(
    req: &mut http::Request,
    max: u64,
    pack: &std::path::Path,
) -> Result<(alt_wire::push::PushHead, u64), (u16, String)> {
    use std::io::Read;
    let gzipped = req
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Encoding") && h.value.as_str().contains("gzip"));
    let over = max.saturating_add(1);
    let mut reader: Box<dyn Read> = if gzipped {
        Box::new(alt_inflate::Reader::new(
            alt_inflate::Format::Gzip,
            req.body(over).take(over),
        ))
    } else {
        Box::new(req.body(over).take(over))
    };
    // the commands are held in memory: they get a cap of their own
    let mut commands = (&mut reader).take(max_command_bytes());
    let head = alt_wire::push::parse_push_request_head(&mut commands, HashAlgo::Sha1)
        .map_err(|e| (limits::unread_commands(&e), format!("{e}")))?;
    let mut tmp = std::fs::File::create(pack).map_err(|e| (500, format!("tempfile: {e}")))?;
    // the cap is on what the body inflates to as well: a gzip body is
    // counted as it is written out. Not synced: the file is gone when
    // the request is over
    let n = std::io::copy(&mut (&mut reader).take(over), &mut tmp)
        .map_err(|e| (limits::refusal(&e), format!("stream pack: {e}")))?;
    Ok((head, n))
}

/// The `report-status` of a push: how the unpack went and, for each
/// command, whether the refs moved or why they did not.
pub(super) fn report_status(
    head: &alt_wire::push::PushHead,
    unpack: &Result<(), String>,
    moved: &Result<(), String>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let status = |u: &alt_wire::push::RefUpdate| match moved {
        Ok(()) => CommandStatus::Ok(u.name.clone()),
        Err(reason) => CommandStatus::Ng {
            name: u.name.clone(),
            reason: reason.clone(),
        },
    };
    let command_status: Vec<CommandStatus> = head.updates.iter().map(status).collect();
    let unpack = unpack.as_ref().map(|_| ()).map_err(|s| s.as_str());
    let mut out = Vec::new();
    match head.capabilities.iter().any(|c| c == "side-band-64k") {
        true => alt_wire::push::encode_report_status_sideband(&mut out, unpack, &command_status)?,
        false => alt_wire::push::encode_report_status(&mut out, unpack, &command_status)?,
    }
    Ok(out)
}

/// POST /git-receive-pack: parse the client's ref-update list
/// and raw pack, ingest objects into the alt odb, then commit the ref
/// changes through `RefStore::commit` so the whole push lands as one
/// atomic op-log entry (mirrors the local-commit path). Reply with a
/// `report-status` body.
pub(super) fn handle_receive_pack(
    repo: &Repository,
    writer: &WriteCoordinator,
    auth_user: Option<&str>,
    mut req: http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = &writer.store;
    let max = max_body_bytes();
    let pack_tmp_dir = alt_os::temp::tempdir()?;
    let pack_tmp_path = pack_tmp_dir.path().join("incoming.pack");
    let parse_outcome = read_push(&mut req, max, &pack_tmp_path);
    let (head, pack_bytes_streamed) = match parse_outcome {
        Ok(t) => t,
        Err((status, reason)) => {
            let r = Response::from_string(reason).with_status_code(StatusCode(status));
            respond_logged(req, r, log)?;
            return Ok(());
        }
    };

    // refresh the cached Store so an operator-edited
    // `.alt/policy` (or fresh oplog ops from another writer) takes
    // effect on the *next* request — no server restart. `users` is
    // already re-read per request inside `check_auth`; `trust` keys
    // are re-read per request inside `verify_push_signature`. policy
    // is the last cached piece, and `Store::refresh()` is the same
    // entry the daemon uses to catch up its read view. Must
    // happen *before* the sig_block decision below so the next
    // signing policy is seen on this request, not a request later.
    if let Err(e) = store.take().settle_told() {
        eprintln!("altd-server: store refresh failed (continuing with stale policy): {e}");
    }

    // pre-commit verify of the wire signature. The
    // signature is computed over the canonical push payload (head's
    // updates + algo); pack bytes don't participate, so we can decide
    // sig_block before reading them.
    let sig_outcome = verify_push_signature(writer, &head)?;
    let principal = pick_principal(&sig_outcome, auth_user);
    let require_signed = require_signed_for(store, &principal.0);
    let sig_block: Option<String> = match (&sig_outcome, require_signed) {
        (SigOutcome::Verified { .. }, _) => None,
        (other, true) => Some(format!("signature required: {}", describe_sig(other))),
        (_, false) => None,
    };

    if pack_bytes_streamed > max {
        let r = Response::from_string(format!("payload too large: pack body exceeds cap {max}"))
            .with_status_code(StatusCode(413));
        respond_logged(req, r, log)?;
        return Ok(());
    }

    // the objects a thin pack is made of deltas of
    let bases = |oid: &ObjectId| {
        let read = repo.read_object(oid);
        let read = read.map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(read.map(|raw| (raw.kind, raw.data)))
    };
    let whole = (pack_tmp_path.as_path(), pack_limits(), &bases as _);
    let pack = (pack_bytes_streamed != 0).then_some(whole);
    // nothing is appended behind what a failed sync left in doubt
    let stopped = writer.group.failed().map(|e| format!("{STOPPED}: {e}"));
    let (unpack_result, moved) = match sig_block.or(stopped) {
        Some(reason) => (Ok(()), Err(reason)),
        None => match receive::take_in(store, pack, &head.updates, &principal, &writer.group) {
            Ok(report) => report,
            Err(e) if receive::busy(&*e) => {
                let busy = "the packs of other pushes are being indexed; send the push again";
                let r = Response::from_string(busy).with_status_code(StatusCode(503));
                return Ok(respond_logged(req, r, log)?);
            }
            Err(e) => return Err(e),
        },
    };
    // wait for the group-commit leader's fsync to cover us, outside the
    // store lock: this is the only place we touch disk durability on the
    // receive-pack path. Other concurrent pushes coalesce into the same
    // flush, so 4-way concurrent traffic pays ~1 fsync instead of 4.
    let moved = moved.and_then(|ticket| {
        let durable = match ticket {
            Some(t) => writer.group.await_durable(&writer.sink, t),
            None => Ok(()),
        };
        log.fsync_seq = Some(writer.group.fsync_count());
        durable.map_err(|e| format!("durability: {e}"))
    });
    let out = report_status(&head, &unpack_result, &moved)?;
    let mut resp = Response::from_data(out);
    resp.add_header(header(
        "Content-Type",
        "application/x-git-receive-pack-result",
    ));
    resp.add_header(header("Cache-Control", "no-cache"));
    respond_logged(req, resp, log)?;
    let _ = repo; // borrow keepalive across response
    Ok(())
}

/// result of verifying the `alt-principal=<id>` +
/// `alt-sig=<ed25519>` capabilities the client may have attached to a
/// push. `NoSignature` is the empty-attribution baseline; the rest are
/// failure modes the policy gate can act on.
#[derive(Debug)]
pub(super) enum SigOutcome {
    Verified { principal_id: String },
    NoSignature,
    BadSignature(String),
    UnknownPrincipal(String),
}

pub(super) fn describe_sig(o: &SigOutcome) -> String {
    match o {
        SigOutcome::Verified { .. } => "verified".into(),
        SigOutcome::NoSignature => "no signature attached".into(),
        SigOutcome::BadSignature(e) => format!("signature did not verify ({e})"),
        SigOutcome::UnknownPrincipal(id) => format!("principal '{id}' not in trust store"),
    }
}

/// Run the signature check against `<alt-dir>/trust/`. Returns
/// `NoSignature` when the client didn't attach the pair (the common
/// path on git-native clients).
pub(super) fn verify_push_signature(
    writer: &WriteCoordinator,
    head: &alt_wire::push::PushHead,
) -> Result<SigOutcome, Box<dyn std::error::Error>> {
    let principal_id = head
        .capabilities
        .iter()
        .find_map(|c| c.strip_prefix(&format!("{}=", alt_wire::CAP_ALT_PRINCIPAL)));
    let sig_text = head
        .capabilities
        .iter()
        .find_map(|c| c.strip_prefix(&format!("{}=", alt_wire::CAP_ALT_SIG)));
    let (principal_id, sig_text) = match (principal_id, sig_text) {
        (Some(p), Some(s)) => (p.to_owned(), s.to_owned()),
        _ => return Ok(SigOutcome::NoSignature),
    };
    let trust = {
        let guard = writer.store.take();
        guard.trust_keys()?
    };
    let Some((_, pubkey)) = trust.iter().find(|(id, _)| id == &principal_id) else {
        return Ok(SigOutcome::UnknownPrincipal(principal_id));
    };
    let sig = match alt_sign::Sig::from_text(&sig_text) {
        Ok(s) => s,
        Err(e) => return Ok(SigOutcome::BadSignature(format!("{e}"))),
    };
    // if the client echoed back an `alt-nonce=<hex>` cap, the
    // signature is over `nonce <hex>\n` + canonical_payload. We must
    // consume the nonce (single-use, anti-replay) and verify against
    // the nonce-prefixed payload. No nonce echo = legacy payload,
    // verified against the no-nonce form for backwards-compat with
    // pre-nonce clients (the `require_nonce_on_sig` policy axis turns
    // that compat off).
    let echoed_nonce = head
        .capabilities
        .iter()
        .find_map(|c| c.strip_prefix(&format!("{}=", alt_wire::CAP_ALT_NONCE)));
    if let Some(nonce) = echoed_nonce {
        if !writer.nonces.consume(nonce) {
            return Ok(SigOutcome::BadSignature(
                "alt-nonce echoed in push not active on this server (replay or expired)".into(),
            ));
        }
        let payload =
            alt_wire::canonical_push_payload_with_nonce(&head.updates, Some(nonce), HashAlgo::Sha1);
        match pubkey.verify(&payload, &sig) {
            Ok(()) => Ok(SigOutcome::Verified { principal_id }),
            Err(e) => Ok(SigOutcome::BadSignature(format!("{e}"))),
        }
    } else {
        let payload = alt_wire::canonical_push_payload(&head.updates, HashAlgo::Sha1);
        match pubkey.verify(&payload, &sig) {
            Ok(()) => Ok(SigOutcome::Verified { principal_id }),
            Err(e) => Ok(SigOutcome::BadSignature(format!("{e}"))),
        }
    }
}

/// Pick the effective principal + signature label for the op-log actor
/// string. A verified signature wins over Basic-auth (it's a stronger
/// claim of identity); otherwise we fall back to the Basic-auth user
/// (or "anonymous") and the label records *why* it isn't verified.
pub(super) fn pick_principal(
    outcome: &SigOutcome,
    auth_user: Option<&str>,
) -> (Principal, &'static str) {
    let (id, label) = match outcome {
        SigOutcome::Verified { principal_id } => (principal_id.clone(), "ed25519"),
        SigOutcome::NoSignature => (auth_user.unwrap_or("anonymous").to_owned(), "none"),
        SigOutcome::BadSignature(_) => (auth_user.unwrap_or("anonymous").to_owned(), "bad"),
        SigOutcome::UnknownPrincipal(_) => (
            auth_user.unwrap_or("anonymous").to_owned(),
            "unknown-principal",
        ),
    };
    (
        Principal {
            kind: PrincipalKind::Human,
            id,
            session: None,
        },
        label,
    )
}

/// Does this principal's policy require a verified signature on every
/// push? If so, an unverified push is short-circuited with `ng
/// signature required` before any objects land.
pub(super) fn require_signed_for(store: &Held<Store>, principal: &Principal) -> bool {
    let guard = store.take();
    guard.capabilities_for(principal).require_signed
}
