//! Git smart HTTP translation. Ref/object authorization and publication remain
//! in the same repository domain and SQL transaction as native requests.
use crate::{Output, Route};
use alt_server_store::{Credential, Error, RefFilter, RefScope, Result, Store};
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

/// The references a request's view is opened with before its body is read: a push
/// reads every one, the fetch advertisement none, and a fetch command all of them
/// unless there are too many to read before knowing which it asks for.
pub(crate) fn ref_filter(route: &Route) -> RefFilter<'static> {
    match route {
        Route::GitAdvertise(_, false) => RefFilter::Nothing,
        Route::GitUpload(_) => RefFilter::Bounded,
        _ => RefFilter::All,
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
    let view = store.pack_view_with(credential, &repository, ref_filter(&route))?;
    execute_git_with_cache(store, credential, route, protocol_v2, source, view, None)
}

pub(crate) fn execute_git_with_cache(
    store: &mut Store,
    credential: &Credential,
    route: Route,
    protocol_v2: bool,
    source: &mut dyn Read,
    view: alt_server_store::PackView,
    room: Option<Verifying>,
) -> Result<Output> {
    match prepared(store, credential, route, protocol_v2, source, view, room)? {
        Step::Done(output) => Ok(output),
        Step::Read(read) => read.answered(),
    }
}

/// A Git request once what it needs of the database is read: its answer, or
/// what reads it from the segments without the database.
pub(crate) enum Step {
    Done(Output),
    Read(Box<Reading>),
}

/// What a read of the segments is answered from.
pub(crate) struct Reading {
    limits: alt_remote_protocol::Limits,
    receive: bool,
    command: Command,
    view: alt_server_store::PackView,
    room: Option<Verifying>,
}

impl Reading {
    pub(crate) fn answered(self) -> Result<Output> {
        let room = self.room.as_ref();
        let sending = room.map(Verifying::share);
        let gone = room.map(|room| &*room.given_up);
        let (limits, receive, command) = (self.limits, self.receive, &self.command);
        crate::git_read::read(limits, receive, command, self.view, sending, gone)
    }
}

/// [`execute_git_with_cache`] up to what is read of the database.
pub(crate) fn prepared(
    store: &mut Store,
    credential: &Credential,
    route: Route,
    protocol_v2: bool,
    source: &mut dyn Read,
    view: alt_server_store::PackView,
    room: Option<Verifying>,
) -> Result<Step> {
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
        let received =
            crate::git_packs::receive(store, credential, &repository, source, view, room);
        return received.map(Step::Done);
    }
    let command = command(source, advertisement, store.limits())?;
    // the first view authorized the caller without the references; this one checks
    // again and reads the ones the command names together with the segments
    let view = match (&command, view.ref_scope) {
        (Command::Refs(request, _), RefScope::Deferred) if request.ref_prefixes.is_empty() => {
            store.pack_view(credential, &repository)?
        }
        (Command::Refs(request, _), RefScope::Deferred) => store.pack_view_with(
            credential,
            &repository,
            RefFilter::Prefixes(&request.ref_prefixes),
        )?,
        (Command::Fetch(request), RefScope::Deferred) => {
            let wants: Vec<String> = request.wants.iter().map(ToString::to_string).collect();
            let tags = request.include_tag;
            store.pack_view_with(
                credential,
                &repository,
                RefFilter::Tips {
                    wants: &wants,
                    tags,
                },
            )?
        }
        _ => view,
    };
    let limits = store.limits();
    Ok(Step::Read(Box::new(Reading {
        limits,
        receive,
        command,
        view,
        room,
    })))
}

/// What a Git request takes of what the instance has for all of them: the memory
/// the packs of pushes are verified in, and until when the pack of one request
/// waits for its part of it; the room on disk the responses to fetches have.
#[derive(Clone)]
pub(crate) struct Verifying {
    pub memory: &'static alt_pack_segment::Memory,
    pub by: std::time::Instant,
    /// set once nobody waits for the answer: the pack is verified no further
    pub given_up: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub files: Option<&'static alt_pack_segment::BaseFiles>,
    /// the room the responses to fetches have on disk, and the address the
    /// caller is counted under there
    pub sending: (std::sync::Arc<alt_shares::Shares>, Option<std::net::IpAddr>),
    /// the worker of the request, given back while a response waits for its turn
    pub worker: Option<crate::http::worker::Slot>,
}

impl Verifying {
    /// What the response takes of the room the responses to fetches have: it
    /// waits for its turn to be large while the answer is waited for, and the
    /// worker of the request serves others meanwhile.
    pub fn share(&self) -> alt_shares::Share {
        let gone = Some(self.given_up.clone());
        let share = self.sending.0.share(self.sending.1).waiting(None, gone);
        match &self.worker {
            Some(worker) => share.aside(worker.aside(self.given_up.clone())),
            None => share,
        }
    }
}

/// Held by whoever waits for the answer to a request. Dropped, when the answer is
/// there, the time for it is over or the caller is gone, it sets its mark.
#[derive(Default)]
pub(crate) struct Waiting(pub std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// What verifying the pack of a push may take. A base held while its deltas are
/// resolved is held as an index of a pack holds it.
pub(crate) fn pack_limits(
    limits: alt_remote_protocol::Limits,
    room: Option<&Verifying>,
) -> alt_pack_segment::Limits {
    alt_pack_segment::Limits {
        memory: room.map(|room| room.memory),
        memory_by: room.map(|room| room.by),
        files: room.and_then(|room| room.files),
        ..alt_pack_segment::Limits::of(
            limits.graph_objects as u64,
            limits.object_bytes,
            inflated_bytes(limits.total_bytes),
        )
    }
}

/// The pack of a push verified against the segments there are, for as long as
/// its answer is waited for. `led` is what of the segments the one who pushes
/// is led to, where that is not all they hold.
pub(crate) fn verified(
    pack: &[u8],
    segments: &alt_pack_segment::SegmentSet,
    limits: alt_remote_protocol::Limits,
    room: Option<&Verifying>,
    led: Option<&alt_pack_segment::Led>,
) -> Result<alt_pack_segment::Ingested> {
    let within = pack_limits(limits, room);
    let given_up = room.map(|room| &room.given_up);
    match (led, given_up) {
        (Some(led), _) => alt_pack_segment::ingest_led(pack, (segments, led), &within, given_up),
        (None, Some(given_up)) => {
            alt_pack_segment::ingest_while_waited_for(pack, segments, &within, given_up)
        }
        (None, None) => alt_pack_segment::ingest(pack, segments, &within),
    }
    .map_err(crate::git_packs::segment_error)
}

/// The least memory the packs of pushes can be verified in: what one pack takes
/// that holds an object as large as one may be.
pub(crate) fn least_verifying_bytes(limits: alt_remote_protocol::Limits) -> u64 {
    alt_pack_segment::least_memory(&pack_limits(limits, None))
}

/// What the objects of a push may inflate to, all together, where its pack may
/// have `pack_bytes`. The packs of long histories inflate to some thirty times
/// their size, and more where large files change a little at a time; this is four
/// times that, and still a bound on what a pack made to be costly has hashed.
pub(crate) fn inflated_bytes(pack_bytes: u64) -> u64 {
    pack_bytes.saturating_mul(128)
}
