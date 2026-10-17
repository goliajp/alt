//! Shared command parsing and machine failure results for direct and daemon use.

use std::ffi::OsString;

use clap::{CommandFactory, FromArgMatches};
use serde_json::{Value, json};

use crate::cli::Cli;
use crate::daemon::Response;

#[derive(Debug, Clone)]
pub struct Failure {
    pub kind: &'static str,
    pub message: String,
    pub outcome: &'static str,
    pub exit_code: u8,
    pub retryable: bool,
    pub operation_id: Option<String>,
    pub details: Box<Value>,
    pub next_action: &'static str,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

impl Failure {
    pub fn refusal(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            outcome: "not_applied",
            exit_code: 1,
            retryable: false,
            operation_id: None,
            details: Box::new(json!({})),
            next_action: "Inspect the reported condition before submitting a new request",
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            exit_code: 2,
            next_action: "Correct the arguments and submit a new request",
            ..Self::refusal("usage", message)
        }
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            outcome: "unknown",
            exit_code: 75,
            next_action: "Query the request and its operations before attempting another write",
            ..Self::refusal("result_unknown", message)
        }
    }

    pub fn pending(operation: impl ToString, message: impl Into<String>) -> Self {
        Self {
            operation_id: Some(operation.to_string()),
            next_action: "Inspect the operation and run `alt operation recover <operation-id>` after reviewing its preconditions",
            ..Self::unknown(message)
        }
    }

    pub fn response(&self, json_mode: bool, request_id: Option<&str>) -> Response {
        let stderr = if json_mode {
            let mut bytes = serde_json::to_vec(&json!({
                "schema_version": 1,
                "error": {
                    "kind": self.kind,
                    "message": self.message,
                    "outcome": self.outcome,
                    "retryable": self.retryable,
                    "request_id": request_id,
                    "operation_id": self.operation_id,
                    "details": self.details,
                    "next_action": self.next_action,
                }
            }))
            .expect("JSON values serialize");
            bytes.push(b'\n');
            bytes
        } else {
            let request = request_id
                .map(|id| format!("\nrequest: {id}"))
                .unwrap_or_default();
            format!("fatal: {}{}\n", self.message, request).into_bytes()
        };
        Response {
            exit_code: self.exit_code,
            stdout: Vec::new(),
            stderr,
        }
    }
}

pub struct Parsed {
    pub cli: Cli,
    pub normalized: Value,
    pub json: bool,
}

/// Parse without exiting or writing to inherited streams. An error path keeps
/// non-UTF-8 argument bytes distinct from the display string.
pub fn parse(args: Vec<OsString>) -> Result<Parsed, Response> {
    let json_mode = args
        .iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "--json");
    for (index, arg) in args.iter().enumerate() {
        if arg.to_str().is_none() {
            let mut failure = Failure::usage("non-UTF-8 arguments are not supported");
            failure.kind = "unsupported_path";
            *failure.details = json!({
                "argument_index": index,
                "bytes_hex": hex(arg.as_encoded_bytes()),
                "encoding": "os_bytes",
            });
            return Err(failure.response(json_mode, None));
        }
    }
    let matches = Cli::command().try_get_matches_from(args).map_err(|error| {
        if matches!(
            error.kind(),
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
        ) {
            Response {
                exit_code: 0,
                stdout: error.to_string().into_bytes(),
                stderr: Vec::new(),
            }
        } else {
            Failure::usage(error.to_string()).response(json_mode, None)
        }
    })?;
    let normalized = canonical_matches(&matches);
    let json_mode = matches_json(&matches);
    let cli = Cli::from_arg_matches(&matches)
        .map_err(|error| Failure::usage(error.to_string()).response(json_mode, None))?;
    Ok(Parsed {
        cli,
        normalized,
        json: json_mode,
    })
}

fn matches_json(matches: &clap::ArgMatches) -> bool {
    matches
        .try_get_one::<bool>("json")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
        || matches
            .subcommand()
            .is_some_and(|(_, sub)| matches_json(sub))
}

fn canonical_matches(matches: &clap::ArgMatches) -> Value {
    let mut args = std::collections::BTreeMap::new();
    for id in matches.ids() {
        if matches!(id.as_str(), "request_id" | "workspace") {
            continue;
        }
        if let Some(values) = matches.get_raw(id.as_str()) {
            args.insert(
                id.as_str(),
                values
                    .map(|value| hex(value.as_encoded_bytes()))
                    .collect::<Vec<_>>(),
            );
        }
    }
    let subcommand = matches.subcommand().map(|(name, sub)| {
        json!({
            "name": name, "arguments": canonical_matches(sub)
        })
    });
    json!({"values": args, "subcommand": subcommand})
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}

pub fn from_error(error: &(dyn std::error::Error + 'static), writing: bool) -> Failure {
    if let Some(failure) = error.downcast_ref::<Failure>() {
        return failure.clone();
    }
    if let Some(error) = error.downcast_ref::<alt_refs::RefError>() {
        match error {
            alt_refs::RefError::CapabilityDenied(_) => {
                return Failure::refusal("capability_denied", error.to_string());
            }
            alt_refs::RefError::Conflict { .. } | alt_refs::RefError::Rejected(_) => {
                return Failure::refusal("precondition_failed", error.to_string());
            }
            _ => {}
        }
    }
    if error.is::<alt_flow::FlowError>() {
        return Failure::usage(error.to_string());
    }
    let mut messages = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        let message = error.to_string();
        if messages.last() != Some(&message) {
            messages.push(message);
        }
        source = error.source();
    }
    if writing {
        Failure::unknown(messages.join(": "))
    } else {
        Failure {
            exit_code: 128,
            ..Failure::refusal("execution_error", messages.join(": "))
        }
    }
}

pub fn bytes_value(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => json!({"encoding": "utf-8", "text": text, "truncated": false}),
        Err(_) => json!({"encoding": "hex", "bytes": hex(bytes), "truncated": false}),
    }
}

/// The operation ids are attributed to this request, never inferred from a
/// shared global last-op. A known refusal after earlier effects is partial.
pub struct ResultContext<'a> {
    pub json_mode: bool,
    pub request_id: Option<&'a str>,
    pub operations: &'a [String],
    pub writing: bool,
    pub conflict_on_failure: bool,
    pub observed_outcome: Option<&'a std::cell::Cell<&'static str>>,
}

pub fn finish(
    result: Result<u8, Box<dyn std::error::Error>>,
    stdout: Vec<u8>,
    diagnostics: Vec<u8>,
    context: ResultContext<'_>,
) -> Response {
    let ResultContext {
        json_mode,
        request_id,
        operations,
        writing,
        conflict_on_failure,
        observed_outcome,
    } = context;
    let mut failure = match result {
        Ok(0) => {
            if let Some(outcome) = observed_outcome {
                outcome.set("applied");
            }
            let mut stdout = stdout;
            if json_mode && (request_id.is_some() || !diagnostics.is_empty()) {
                let mut value = match serde_json::from_slice::<Value>(&stdout) {
                    Ok(Value::Object(value)) => value,
                    _ => {
                        let mut value = serde_json::Map::new();
                        value.insert("schema_version".into(), json!(1));
                        value.insert("command_output".into(), bytes_value(&stdout));
                        value
                    }
                };
                value.insert("request_id".into(), json!(request_id));
                value.insert("operation_ids".into(), json!(operations));
                if !diagnostics.is_empty() {
                    value.insert("diagnostics".into(), bytes_value(&diagnostics));
                }
                stdout = serde_json::to_vec(&value).expect("JSON values serialize");
                stdout.push(b'\n');
            }
            return Response {
                exit_code: 0,
                stdout,
                stderr: if json_mode { Vec::new() } else { diagnostics },
            };
        }
        Ok(code) => {
            let mut failure = Failure::refusal(
                "validation_failed",
                "command reported a conflict or failed validation",
            );
            failure.exit_code = code;
            if conflict_on_failure {
                failure.kind = "conflict";
                failure.outcome = "partial";
            }
            failure
        }
        Err(error) => from_error(error.as_ref(), writing),
    };
    if failure.outcome == "not_applied" && !operations.is_empty() {
        failure.outcome = "partial";
    }
    if failure.operation_id.is_none() {
        failure.operation_id = operations.last().cloned();
    }
    let details = failure
        .details
        .as_object_mut()
        .expect("failure details object");
    details.insert("operation_ids".into(), json!(operations));
    if !stdout.is_empty() {
        details.insert("command_output".into(), bytes_value(&stdout));
    }
    if !diagnostics.is_empty() {
        details.insert("diagnostics".into(), bytes_value(&diagnostics));
    }
    if let Some(outcome) = observed_outcome {
        outcome.set(failure.outcome);
    }
    let mut response = failure.response(json_mode, request_id);
    if !json_mode {
        response.stdout = stdout;
        let mut stderr = diagnostics;
        stderr.extend(response.stderr);
        response.stderr = stderr;
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_refusal_is_one_json_value_on_stderr() {
        let response = Failure::refusal("precondition_failed", "文件「日本語」已改变")
            .response(true, Some("00112233445566778899aabbccddeeff"));
        assert!(response.stdout.is_empty());
        assert_eq!(response.exit_code, 1);
        let value: Value = serde_json::from_slice(&response.stderr).unwrap();
        assert_eq!(value["error"]["message"], "文件「日本語」已改变");
        assert_eq!(value["error"]["outcome"], "not_applied");
        assert_eq!(
            value["error"]["request_id"],
            "00112233445566778899aabbccddeeff"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments_are_preserved_without_parsing_a_lossy_path() {
        use std::os::unix::ffi::OsStringExt;
        let response = parse(vec![
            "alt".into(),
            "add".into(),
            OsString::from_vec(vec![b'a', 0xff]),
            "--json".into(),
        ])
        .err()
        .unwrap();
        assert_eq!(response.exit_code, 2);
        assert!(response.stdout.is_empty());
        let value: Value = serde_json::from_slice(&response.stderr).unwrap();
        assert_eq!(value["error"]["details"]["bytes_hex"], "61ff");
    }

    #[test]
    fn conflicting_command_retains_its_result_and_reports_partial_effects() {
        let response = finish(
            Ok(1),
            b"{\"conflicts\":[\"a.txt\"]}\n".to_vec(),
            b"diagnostic\n".to_vec(),
            ResultContext {
                json_mode: true,
                request_id: Some("id"),
                operations: &[],
                writing: true,
                conflict_on_failure: true,
                observed_outcome: None,
            },
        );
        assert!(response.stdout.is_empty());
        let value: Value = serde_json::from_slice(&response.stderr).unwrap();
        assert_eq!(value["error"]["kind"], "conflict");
        assert_eq!(value["error"]["outcome"], "partial");
        assert_eq!(
            value["error"]["details"]["command_output"]["text"],
            "{\"conflicts\":[\"a.txt\"]}\n"
        );
        assert_eq!(
            value["error"]["details"]["diagnostics"]["text"],
            "diagnostic\n"
        );
    }
}
