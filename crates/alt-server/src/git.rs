//! Git smart HTTP translation. Ref/object authorization and publication remain
//! in the same repository domain and SQL transaction as native requests.
use crate::{Output, Route};
use alt_server_store::{Credential, Error, Result, Store};
use alt_wire::pkt::{self, Frame};
use std::io::{Cursor, Read};
fn validate_command(raw: &[u8], max_lines: usize) -> Result<(String, bool)> {
    let mut input = Cursor::new(raw);
    let mut scratch = Vec::new();
    let mut command = None;
    let mut count = 0usize;
    loop {
        count += 1;
        if count > max_lines {
            return Err(Error::Limit("Git command lines"));
        }
        match pkt::read_frame(&mut input, &mut scratch)
            .map_err(|_| Error::Integrity("Git command framing"))?
        {
            Frame::Delim => break,
            Frame::Data(line) => {
                let line = std::str::from_utf8(line)
                    .map_err(|_| Error::Integrity("Git header encoding"))?
                    .trim_end_matches('\n');
                if let Some(value) = line.strip_prefix("command=") {
                    if command.is_some() || !matches!(value, "ls-refs" | "fetch") {
                        return Err(Error::Precondition("unsupported Git command"));
                    }
                    command = Some(value.to_owned());
                } else if line != "object-format=sha1" && !line.starts_with("agent=") {
                    return Err(Error::Precondition("unsupported Git command capability"));
                }
            }
            _ => return Err(Error::Integrity("Git command delimiter")),
        }
    }
    let command = command.ok_or(Error::Integrity("missing Git command"))?;
    let mut unborn = false;
    loop {
        count += 1;
        if count > max_lines {
            return Err(Error::Limit("Git command lines"));
        }
        match pkt::read_frame(&mut input, &mut scratch)
            .map_err(|_| Error::Integrity("Git argument framing"))?
        {
            Frame::Flush => break,
            Frame::Data(line) => {
                let line = std::str::from_utf8(line)
                    .map_err(|_| Error::Integrity("Git argument encoding"))?
                    .trim_end_matches('\n');
                unborn |= command == "ls-refs" && line == "unborn";
                let supported = match command.as_str() {
                    "ls-refs" => {
                        matches!(line, "peel" | "symrefs" | "unborn")
                            || line.starts_with("ref-prefix ")
                    }
                    "fetch" => {
                        matches!(
                            line,
                            "done"
                                | "no-progress"
                                | "thin-pack"
                                | "ofs-delta"
                                | "include-tag"
                                | "wait-for-done"
                        ) || line.starts_with("want ")
                            || line.starts_with("have ")
                    }
                    _ => false,
                };
                if !supported {
                    return Err(Error::Precondition(
                        "unsupported Git fetch option; shallow and filtered fetch are unavailable",
                    ));
                }
            }
            _ => return Err(Error::Integrity("Git argument terminator")),
        }
    }
    if input.position() != raw.len() as u64 {
        return Err(Error::Integrity("trailing Git command bytes"));
    }
    Ok((command, unborn))
}
pub(crate) enum Command {
    Advertisement,
    Refs(alt_wire::ls_refs::LsRefsRequest, bool),
    Fetch(alt_wire::fetch::FetchRequest),
}
fn command(
    source: &mut dyn Read,
    advertisement: bool,
    limits: alt_remote_protocol::Limits,
) -> Result<Command> {
    if advertisement {
        if source.read(&mut [0u8; 1])? != 0 {
            return Err(Error::Integrity("unexpected advertisement body"));
        }
        return Ok(Command::Advertisement);
    }
    let mut raw = Vec::new();
    source
        .take((limits.request_bytes as u64).saturating_add(1))
        .read_to_end(&mut raw)?;
    if raw.len() > limits.request_bytes {
        return Err(Error::Limit("Git command body"));
    }
    let (name, unborn) = validate_command(
        &raw,
        limits.graph_objects.saturating_mul(2).saturating_add(100),
    )?;
    if name == "ls-refs" {
        let (request, _) = alt_wire::ls_refs::parse_ls_refs_request(&mut Cursor::new(&raw))
            .map_err(|_| Error::Integrity("Git ls-refs request"))?;
        Ok(Command::Refs(request, unborn))
    } else {
        let (request, _) = alt_wire::fetch::parse_fetch_request(&mut Cursor::new(&raw))
            .map_err(|_| Error::Integrity("Git fetch request"))?;
        Ok(Command::Fetch(request))
    }
}

/// Runs one Git smart HTTP request for an already authenticated caller.
pub fn execute_git(
    store: &mut Store,
    credential: &Credential,
    route: Route,
    protocol_v2: bool,
    source: &mut dyn Read,
) -> Result<Output> {
    let repository = match &route {
        Route::GitAdvertise(repository, _)
        | Route::GitUpload(repository)
        | Route::GitReceive(repository) => repository.clone(),
        _ => return Err(Error::Precondition("not a Git endpoint")),
    };
    let view = store.pack_view(credential, &repository)?;
    execute_git_with_cache(store, credential, route, protocol_v2, source, view)
}

pub(crate) fn execute_git_with_cache(
    store: &mut Store,
    credential: &Credential,
    route: Route,
    protocol_v2: bool,
    source: &mut dyn Read,
    view: alt_server_store::PackView,
) -> Result<Output> {
    let (repository, receive, advertisement) = match &route {
        Route::GitAdvertise(repository, receive) => (repository.clone(), *receive, true),
        Route::GitUpload(repository) => (repository.clone(), false, false),
        Route::GitReceive(repository) => (repository.clone(), true, false),
        _ => return Err(Error::Precondition("not a Git endpoint")),
    };
    if !receive && !protocol_v2 {
        return Err(Error::Precondition("Git fetch protocol v2 is required"));
    }
    if receive && !advertisement {
        if !view.writable {
            return Err(Error::Unauthorized);
        }
        return crate::git_packs::receive(store, credential, &repository, source, view);
    }
    let command = command(source, advertisement, store.limits())?;
    crate::git_read::read(store.limits(), receive, &command, view)
}
