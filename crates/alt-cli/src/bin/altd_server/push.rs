//! A push: its body read, its signature looked at, its report written.

use super::*;

#[path = "push/signature.rs"]
mod signature;

use signature::*;

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
    told: &receive::Told,
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
    let mut tmp = std::fs::File::create(pack).map_err(|e| (500, told("tempfile", &e)))?;
    // the cap is on what the body inflates to as well: a gzip body is
    // counted as it is written out. Not synced: the file is gone when
    // the request is over
    let n = std::io::copy(&mut (&mut reader).take(over), &mut tmp).map_err(|e| {
        match limits::refusal(&e) {
            // the server had no room for it
            503 => (503, told("stream pack", &e)),
            status => (status, format!("stream pack: {e}")),
        }
    })?;
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
    writer: &WriteCoordinator,
    auth_user: Option<&str>,
    mut req: http::Request,
    log: &mut LogCtx,
    repo: Reading,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = &writer.store;
    let max = max_body_bytes();
    let pack_tmp_dir = alt_os::temp::tempdir()?;
    let pack_tmp_path = pack_tmp_dir.path().join("incoming.pack");
    let (signed_in, told) = (auth_user.is_some(), limits::telling(auth_user.is_some()));
    let parse_outcome = read_push(&mut req, max, &pack_tmp_path, &told);
    let (head, pack_bytes_streamed) = match parse_outcome {
        Ok(t) => t,
        Err((status, reason)) => {
            let r = Response::from_string(reason).with_status_code(StatusCode(status));
            repo.answer(req, r, log)?;
            return Ok(());
        }
    };

    // refresh the cached Store so an operator-edited `.alt/policy` (or
    // fresh oplog ops from another writer) takes effect without a restart,
    // before the sig_block decision below sees it: `users` and `trust` are
    // re-read per request already, and policy is the last cached piece
    if let Err(e) = store.take().settle_told() {
        eprintln!("altd-server: store refresh failed (continuing with stale policy): {e}");
    }

    let (principal, sig_block) = signed(writer, &head, &req, auth_user)?;

    if pack_bytes_streamed > max {
        let r = Response::from_string(format!("payload too large: pack body exceeds cap {max}"))
            .with_status_code(StatusCode(413));
        repo.answer(req, r, log)?;
        return Ok(());
    }

    // the pack is taken in for minutes where it is large or waits for memory:
    // a client that asked for the side band hears from the server meanwhile
    let banded = head.capabilities.iter().any(|c| c == "side-band-64k");
    if banded {
        req.begin(result_headers(), Vec::new(), super::told::told().keepalive);
    }

    // the objects a thin pack is made of deltas of
    let bases = |oid: &ObjectId| {
        let read = repo.read_object(oid);
        let read = read.map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(read.map(|raw| (raw.kind, raw.data)))
    };
    let whole = (pack_tmp_path.as_path(), pack_limits(&req), &bases as _);
    let pack = (pack_bytes_streamed != 0).then_some(whole);
    // nothing is appended behind what a failed sync left in doubt
    let stopped = writer.group.failed().map(|e| told(STOPPED, &*Box::from(e)));
    let taken =
        |pack| receive::take_in(store, pack, &head.updates, &principal, &writer.group, &told);
    let (unpack_result, moved) = match sig_block.or(stopped) {
        Some(reason) => (Ok(()), Err(reason)),
        None => match taken(pack) {
            Ok(report) => report,
            Err(e) if receive::busy(&*e) => {
                return Ok(repo.answer(req, limits::busy(), log)?);
            }
            Err(e) => return not_taken_in(req, repo, log, (e, signed_in)),
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
        durable.map_err(|e| told("durability", &*Box::from(e)))
    });
    let out = report_status(&head, &unpack_result, &moved)?;
    let mut resp = Response::from_data(out);
    result_headers()
        .into_iter()
        .for_each(|h| resp.add_header(h));
    repo.answer(req, resp, log)?;
    Ok(())
}

/// Answers a push whose pack an error of the server kept from being taken
/// in, in the side band where the head went out. A peer that is not
/// `signed_in` is told only the kind of the error, which is the handler's
/// too.
fn not_taken_in(
    req: http::Request,
    repo: Reading,
    log: &mut LogCtx,
    (e, signed_in): (Box<dyn std::error::Error>, bool),
) -> Result<(), Box<dyn std::error::Error>> {
    let why = limits::told_of(&*e, "the server failed to take the push in", signed_in);
    repo.answer(req, Response::from_string(why).with_status_code(500), log)?;
    Err(e)
}

/// The headers of what `receive-pack` answers a push with.
fn result_headers() -> Vec<http::Header> {
    let content = header("Content-Type", "application/x-git-receive-pack-result");
    vec![content, header("Cache-Control", "no-cache")]
}
