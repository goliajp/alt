//! Shared request preparation, command capture, and durable result publication.
use crate::cli::{self, Command};
use crate::daemon::{RequestId, Response};
use crate::hardening;
use crate::machine::{self, Failure, Parsed};
use crate::native::{self, Identity, Store};
use crate::request::{self, Claim, Claimed};
use alt_json::json;
use std::path::{Path, PathBuf};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

pub struct Execution {
    pub parsed: Parsed,
    pub identity: Identity,
    pub id: Option<RequestId>,
    pub cwd: PathBuf,
    pub writing: bool,
    /// the request id was drawn at random for this very run: no operation
    /// recorded before it can carry it
    drawn: bool,
    /// for as long as the command runs, its thread leaves what it writes
    /// unsynced where the command does ([`hardening::enter`])
    _left: Option<alt_os::durability::Unsynced>,
    /// warnings of the configuration the command syncs by, told once it runs
    said: Option<String>,
    claim: Option<Claim>,
    outcome: std::cell::Cell<&'static str>,
}

/// The control directory a command runs in, the repository and the
/// workspace by their ids, and what the command is bound to.
pub type Bound = (PathBuf, String, String, alt_json::Value);

pub fn binding(parsed: &Parsed, cwd: &Path, identity: &Identity) -> Res<Bound> {
    let (alt_dir, coord) = native::resolve_workspace(cwd, parsed.cli.workspace.as_deref())?;
    let repository = request::repository_identity(&alt_dir)?;
    let workspace = coord.request_workspace()?;
    let cwd = std::fs::canonicalize(cwd)?;
    let root = std::fs::canonicalize(coord.root())?;
    let relative = cwd.strip_prefix(&root).unwrap_or(&cwd);
    let value = json!({"version":1,"arguments":parsed.normalized(),"repository":repository,
        "workspace":workspace,"cwd_bytes":machine::hex(relative.as_os_str().as_encoded_bytes()),"identity":identity.binding()});
    Ok((alt_dir, repository, workspace, value))
}

/// Gives a native write the request id it runs under and records that it
/// was submitted. Where the caller named none, one is drawn and put among
/// `args`. Returns the command as it parses then, what it is bound to,
/// and whether the id was drawn here.
pub fn submit(
    args: &mut Vec<std::ffi::OsString>,
    mut parsed: Parsed,
    cwd: &Path,
    identity: &Identity,
) -> Result<(Parsed, Bound, bool), Response> {
    let drawn = parsed.cli.request_id.is_none();
    let id = match parsed.cli.request_id.as_deref() {
        Some(id) => request::parse_id(id).map_err(|e| e.response(parsed.json, Some(id)))?,
        None => request::new_id()
            .map_err(|e| machine::from_error(&e, false).response(parsed.json, None))?,
    };
    if drawn {
        args.splice(1..1, ["--request-id".into(), machine::hex(&id).into()]);
        parsed = machine::parse(args.clone())?;
    }
    let bound = binding(&parsed, cwd, identity)
        .and_then(|bound| {
            // this process runs the one command: what it says here stands
            let at = (cwd, parsed.cli.workspace.as_deref());
            let left = hardening::enter(&parsed.cli.command, drawn, &bound.0, identity, at)?;
            std::mem::forget(left);
            // an id drawn here for a command this process runs itself is
            // known to nobody until the request is claimed under it
            if !(drawn && crate::client::runs_here(&parsed.cli.command)) {
                request::record_submission(&bound.0, id, &bound.3)?;
            }
            Ok(bound)
        })
        .map_err(|error| {
            machine::from_error(error.as_ref(), false)
                .response(parsed.json, Some(&machine::hex(&id)))
        })?;
    Ok((parsed, bound, drawn))
}

impl Execution {
    /// Refuses a request id an older alt recorded effects under without a
    /// receipt. An id drawn for this run is on no operation older than it.
    fn ensure_not_legacy(&self, store: &Store) -> Res<()> {
        match self.id {
            Some(id) if !self.drawn && store.legacy_request(&id)? => Err(Failure::unknown("legacy request has recorded effects but no complete response; inspect operations before recovery").into()),
            _ => Ok(()),
        }
    }

    /// Claim before acquiring any daemon store mutex. A retry can block here
    /// while the original publishes its result, without holding up durability.
    /// `drawn`: the request id was drawn for this run ([`submit`]).
    /// `bound`: what the command is bound to, where this process worked
    /// it out when it submitted the command.
    pub fn prepare(
        parsed: Parsed,
        cwd: PathBuf,
        identity: Identity,
        wire_id: Option<RequestId>,
        drawn: bool,
        bound: Option<Bound>,
    ) -> Result<Self, Response> {
        let writing = cli::mutates_command(&parsed.cli.command);
        let reported_id = parsed
            .cli
            .request_id
            .clone()
            .or_else(|| wire_id.map(|id| machine::hex(&id)));
        let result = (|| -> Res<_> {
            let explicit = parsed
                .cli
                .request_id
                .as_deref()
                .map(request::parse_id)
                .transpose()?;
            if explicit.is_some() && wire_id.is_some() && explicit != wire_id {
                return Err(Failure::usage("wire and argument request ids disagree").into());
            }
            let id = explicit.or(wire_id);
            let native_write = writing && cli::is_native(&parsed.cli.command);
            if id.is_some() && !native_write {
                return Err(Failure::refusal(
                    "unsupported_request",
                    "request identities require a write in an existing native repository",
                )
                .into());
            }
            if native_write {
                let id = match id {
                    Some(id) => id,
                    None => request::new_id()?,
                };
                let (alt_dir, repository, workspace, value) = match bound {
                    Some(bound) => bound,
                    None => binding(&parsed, &cwd, &identity)?,
                };
                let at = (cwd.as_path(), parsed.cli.workspace.as_deref());
                let left = hardening::enter(&parsed.cli.command, drawn, &alt_dir, &identity, at)?;
                // what was found to say of `core.fsync`, here or in `submit`
                let left = (left, hardening::take_warnings());
                match request::claim(&alt_dir, id, repository, workspace, &value)? {
                    Claimed::Execute(claim) => Ok((Some(id), Some((claim, left)), None)),
                    Claimed::Replay(response) => Ok((Some(id), None, Some(response))),
                }
            } else {
                Ok((None, None, None))
            }
        })();
        let outcome = std::cell::Cell::new("unknown");
        match result {
            Ok((_, _, Some(response))) => Err(response),
            Ok((id, claimed, None)) => {
                let (claim, left) = claimed.unzip();
                let (_left, said) = left.unzip();
                Ok(Self {
                    parsed,
                    identity,
                    id,
                    cwd,
                    writing,
                    drawn,
                    _left,
                    said,
                    claim,
                    outcome,
                })
            }
            Err(error) => Err(machine::from_error(error.as_ref(), writing)
                .response(parsed.json, reported_id.as_deref())),
        }
    }

    pub fn failure(&self, error: &(dyn std::error::Error + 'static)) -> Response {
        let id = self.id.as_ref().map(|id| machine::hex(id));
        // a store that had cut something before it failed tells of it here
        let Some(after) = error.downcast_ref::<native::AfterDiscards>() else {
            return machine::from_error(error, self.writing)
                .response(self.parsed.json, id.as_deref());
        };
        let mut failure = machine::from_error(after.cause.as_ref(), self.writing);
        if self.parsed.json {
            let details = failure.details.as_object_mut().expect("details object");
            details.insert(
                "diagnostics".into(),
                machine::bytes_value(after.said.as_bytes()),
            );
        }
        let mut response = failure.response(self.parsed.json, id.as_deref());
        if !self.parsed.json {
            response.stderr.splice(0..0, after.said.bytes());
        }
        response
    }

    pub fn on_store(
        &self,
        store: &mut Store,
        repo: &alt_repo::Repository,
    ) -> (Response, Vec<String>) {
        let capture = crate::diagnostics::Capture::start(self.parsed.json);
        let _caller = self.identity.outer_environment();
        let _ = crate::diagnostics::emit(self.said.as_deref().unwrap_or("").as_bytes());
        // an id drawn for this run is on no operation older than the run
        let began = self.drawn.then(|| store.log_anchor());
        let mut stdout = Vec::new();
        let result = (|| -> Res<u8> {
            let (directory, _) =
                native::resolve_workspace(&self.cwd, self.parsed.cli.workspace.as_deref())?;
            // the same spelling names the same directory
            if directory != store.alt_dir()
                && std::fs::canonicalize(directory)? != std::fs::canonicalize(store.alt_dir())?
            {
                return Err(Failure::refusal(
                    "repository_mismatch",
                    "request repository does not match this daemon",
                )
                .into());
            }
            self.ensure_not_legacy(store)?;
            let ran = cli::run_on_store(
                &self.parsed.cli,
                store,
                repo,
                &self.cwd,
                &self.identity,
                self.id,
                &mut stdout,
            );
            store.keep_checked(ran.is_ok());
            // what the store cut off its files for this command, or before it
            store.warn_discarded()?;
            ran
        })();
        let operations = self
            .id
            .map(|id| store.request_operations_since(began, &machine::hex(&id)))
            .transpose();
        let (result, operations) = match operations {
            Ok(operations) => (result, operations.unwrap_or_default()),
            Err(error) => (Err(error), Vec::new()),
        };
        (
            self.finish(result, stdout, capture.finish(), &operations),
            operations,
        )
    }

    pub fn finish(
        &self,
        result: Res<u8>,
        stdout: Vec<u8>,
        diagnostics: Vec<u8>,
        operations: &[String],
    ) -> Response {
        machine::finish(
            result,
            stdout,
            diagnostics,
            machine::ResultContext {
                json_mode: self.parsed.json,
                request_id: self.id.as_ref().map(|id| machine::hex(id)).as_deref(),
                operations,
                writing: self.writing,
                observed_outcome: Some(&self.outcome),
                conflict_on_failure: matches!(
                    self.parsed.cli.command,
                    Command::Merge { .. }
                        | Command::CherryPick { .. }
                        | Command::Rebase { .. }
                        | Command::Revert { .. }
                ),
            },
        )
    }

    /// Invoke only after the data/log barrier. On publication failure the
    /// original intent remains pending and no caller receives a success ack.
    pub fn publish(mut self, response: Response, operations: Vec<String>) -> Response {
        if let Some(claim) = self.claim.take()
            && let Err(error) = claim.complete(&response, operations, self.outcome.get())
        {
            let mut failure = machine::from_error(error.as_ref(), true);
            failure.outcome = "unknown";
            failure.exit_code = 75;
            return failure.response(
                self.parsed.json,
                self.id.as_ref().map(|id| machine::hex(id)).as_deref(),
            );
        }
        response
    }
}

/// A command that works on no repository: it writes what it prints.
pub type OwnRun<'a> = &'a dyn Fn(&mut Vec<u8>) -> Result<(), Box<dyn std::error::Error>>;

/// Runs a command that keeps a journal of its own and works on no
/// repository: it takes neither a request id nor a workspace.
pub fn outside_repository(parsed: &Parsed, refused: &str, run: OwnRun) -> Response {
    if parsed.cli.request_id.is_some() || parsed.cli.workspace.is_some() {
        // a runner's refusal names the request it was given
        let id = match &parsed.cli.command {
            Command::Runner { .. } => parsed.cli.request_id.as_deref(),
            _ => None,
        };
        return Failure::usage(refused).response(parsed.json, id);
    }
    let mut stdout = Vec::new();
    match run(&mut stdout) {
        Ok(()) => Response {
            exit_code: 0,
            stdout,
            stderr: Vec::new(),
        },
        Err(error) => machine::from_error(error.as_ref(), false).response(parsed.json, None),
    }
}

/// Runs a command as any other is run.
pub type Attempt<'a> = Box<dyn FnOnce() -> Response + 'a>;

/// How a command a shell runs on its own is answered: it gets no error
/// and nothing on stderr, whatever state the repository is in. `None`
/// for every other command.
pub fn for_shell<'a>(command: &Command) -> Option<Box<dyn FnOnce(Attempt<'a>) -> Response>> {
    match command {
        Command::Complete(asked) => {
            let asked = asked.clone();
            Some(Box::new(move |attempt| {
                crate::complete::respond(&asked, attempt)
            }))
        }
        Command::Prompt(asked) => {
            let asked = asked.clone();
            Some(Box::new(move |attempt| {
                crate::prompt::respond(&asked, attempt)
            }))
        }
        Command::Shell(asked) => {
            let asked = asked.clone();
            Some(Box::new(move |_| crate::shell::respond(&asked)))
        }
        _ => None,
    }
}
