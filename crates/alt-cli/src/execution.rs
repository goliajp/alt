//! Shared request preparation, command capture, and durable result publication.
use crate::cli::{self, Command};
use crate::daemon::{RequestId, Response};
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
    claim: Option<Claim>,
    outcome: std::cell::Cell<&'static str>,
}

pub fn binding(
    parsed: &Parsed,
    cwd: &Path,
    identity: &Identity,
) -> Res<(PathBuf, String, String, alt_json::Value)> {
    let (alt_dir, coord) = native::resolve_workspace(cwd, parsed.cli.workspace.as_deref())?;
    let repository = request::repository_identity(&alt_dir)?;
    let workspace = coord.request_workspace()?;
    let cwd = std::fs::canonicalize(cwd)?;
    let root = std::fs::canonicalize(coord.root())?;
    let relative = cwd.strip_prefix(&root).unwrap_or(&cwd);
    let value = json!({"version":1,"arguments":parsed.normalized,"repository":repository,
        "workspace":workspace,"cwd_bytes":machine::hex(relative.as_os_str().as_encoded_bytes()),"identity":identity.binding()});
    Ok((alt_dir, repository, workspace, value))
}

impl Execution {
    /// Claim before acquiring any daemon store mutex. A retry can block here
    /// while the original publishes its result, without holding up durability.
    pub fn prepare(
        parsed: Parsed,
        cwd: PathBuf,
        identity: Identity,
        wire_id: Option<RequestId>,
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
                let (alt_dir, repository, workspace, value) = binding(&parsed, &cwd, &identity)?;
                match request::claim(&alt_dir, id, repository, workspace, &value)? {
                    Claimed::Execute(claim) => Ok((Some(id), Some(claim), None)),
                    Claimed::Replay(response) => Ok((Some(id), None, Some(response))),
                }
            } else {
                Ok((None, None, None))
            }
        })();
        match result {
            Ok((_, _, Some(response))) => Err(response),
            Ok((id, claim, None)) => Ok(Self {
                parsed,
                identity,
                id,
                cwd,
                writing,
                claim,
                outcome: std::cell::Cell::new("unknown"),
            }),
            Err(error) => Err(machine::from_error(error.as_ref(), writing)
                .response(parsed.json, reported_id.as_deref())),
        }
    }

    pub fn failure(&self, error: &(dyn std::error::Error + 'static)) -> Response {
        machine::from_error(error, self.writing).response(
            self.parsed.json,
            self.id.as_ref().map(|id| machine::hex(id)).as_deref(),
        )
    }

    pub fn on_store(
        &self,
        store: &mut Store,
        repo: &alt_repo::Repository,
    ) -> (Response, Vec<String>) {
        let capture = crate::diagnostics::Capture::start(self.parsed.json);
        let mut stdout = Vec::new();
        let result = (|| -> Res<u8> {
            let (directory, _) =
                native::resolve_workspace(&self.cwd, self.parsed.cli.workspace.as_deref())?;
            if std::fs::canonicalize(directory)? != std::fs::canonicalize(store.alt_dir())? {
                return Err(Failure::refusal(
                    "repository_mismatch",
                    "request repository does not match this daemon",
                )
                .into());
            }
            if let Some(id) = self.id
                && store.legacy_request(&id)?
            {
                return Err(Failure::unknown("legacy request has recorded effects but no complete response; inspect operations before recovery").into());
            }
            cli::run_on_store(
                &self.parsed.cli,
                store,
                repo,
                &self.cwd,
                self.identity.clone(),
                self.id,
                &mut stdout,
            )
        })();
        let operations = self
            .id
            .map(|id| store.request_operations(&machine::hex(&id)))
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
