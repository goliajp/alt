//! Local daemon (D1: wire protocol + framing).
//!
//! The `alt` client and the `altd` daemon speak this over a Unix socket. A
//! request carries the command's argv, the client's working directory (so
//! workspace inference still works), and the few env vars the command reads;
//! the daemon runs the command and returns captured stdout/stderr/exit. So all
//! command logic is reused verbatim — the daemon is a perf cache, not a fork
//! of behaviour. Coherence with direct `alt` invocations is handled by the
//! store's own concurrency machinery (the daemon catches up per request).

use std::io::{self, Read, Write};
use std::path::PathBuf;

/// The env vars a command may read; the client forwards exactly these so the
/// daemon runs with the caller's identity/options, not its own.
pub const FORWARDED_ENV: &[&str] = &[
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "USER",
    // A5a structured principal: kind/id distinguish agent runs from human
    // logins, session correlates a multi-step agent run in the op log.
    "ALT_PRINCIPAL_KIND",
    "ALT_PRINCIPAL_ID",
    "ALT_SESSION_ID",
    "ALT_RELAXED_DURABILITY",
];

/// A client-chosen idempotency token: 16 bytes, unique per command invocation
/// and stable across that invocation's retries. The daemon stamps it on the
/// command's ref transaction and, on a same-id retry, detects the write as
/// already applied instead of running it twice (D5c, exactly-once). Structurally
/// identical to `alt_refs::IdemKey`.
pub type RequestId = [u8; 16];

/// A command to run: the alt argv (without the program name), the client's
/// working directory, the forwarded env vars, and an optional idempotency id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Exactly-once token for a non-idempotent write (D5c). `None` for reads,
    /// for the direct CLI path, and for an old client whose frame carries no
    /// trailing id field (the daemon then degrades to at-most-once).
    pub id: Option<RequestId>,
}

/// The captured result of running a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub exit_code: u8,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    out.extend_from_slice(b);
}

fn get_u32(buf: &[u8], at: &mut usize) -> io::Result<u32> {
    let end = at
        .checked_add(4)
        .filter(|e| *e <= buf.len())
        .ok_or_else(truncated)?;
    let v = u32::from_le_bytes(buf[*at..end].try_into().unwrap());
    *at = end;
    Ok(v)
}

fn get_bytes(buf: &[u8], at: &mut usize) -> io::Result<Vec<u8>> {
    let len = get_u32(buf, at)? as usize;
    let end = at
        .checked_add(len)
        .filter(|e| *e <= buf.len())
        .ok_or_else(truncated)?;
    let b = buf[*at..end].to_vec();
    *at = end;
    Ok(b)
}

fn get_string(buf: &[u8], at: &mut usize) -> io::Result<String> {
    String::from_utf8(get_bytes(buf, at)?).map_err(|_| bad("non-utf8 string in request"))
}

fn truncated() -> io::Error {
    bad("truncated daemon message")
}

fn bad(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.args.len() as u32).to_le_bytes());
        for a in &self.args {
            put_bytes(&mut out, a.as_bytes());
        }
        put_bytes(&mut out, self.cwd.to_string_lossy().as_bytes());
        out.extend_from_slice(&(self.env.len() as u32).to_le_bytes());
        for (k, v) in &self.env {
            put_bytes(&mut out, k.as_bytes());
            put_bytes(&mut out, v.as_bytes());
        }
        // trailing optional id: a presence byte, then the 16 bytes if present.
        // An old daemon stops decoding after `env` and ignores these extra
        // bytes; a new daemon reading an old (untrailed) frame sees no id.
        match self.id {
            Some(id) => {
                out.push(1);
                out.extend_from_slice(&id);
            }
            None => out.push(0),
        }
        out
    }

    pub fn decode(buf: &[u8]) -> io::Result<Request> {
        let mut at = 0;
        let n = get_u32(buf, &mut at)? as usize;
        let mut args = Vec::with_capacity(n);
        for _ in 0..n {
            args.push(get_string(buf, &mut at)?);
        }
        let cwd = PathBuf::from(get_string(buf, &mut at)?);
        let m = get_u32(buf, &mut at)? as usize;
        let mut env = Vec::with_capacity(m);
        for _ in 0..m {
            let k = get_string(buf, &mut at)?;
            let v = get_string(buf, &mut at)?;
            env.push((k, v));
        }
        // trailing optional id (see `encode`): absent on an old client's frame
        let id = match buf.get(at) {
            None | Some(0) => None, // old client (no field) or explicit absent
            Some(_) => {
                let start = at + 1;
                let end = start
                    .checked_add(16)
                    .filter(|e| *e <= buf.len())
                    .ok_or_else(truncated)?;
                let mut id = [0u8; 16];
                id.copy_from_slice(&buf[start..end]);
                Some(id)
            }
        };
        Ok(Request { args, cwd, env, id })
    }

    /// Builds a request from the current process's argv tail, cwd, the
    /// forwarded env vars that are set, and an optional idempotency `id` (set
    /// by the client for a non-idempotent write, `None` for a read).
    pub fn from_env(args: Vec<String>, id: Option<RequestId>) -> io::Result<Request> {
        let cwd = std::env::current_dir()?;
        let env = FORWARDED_ENV
            .iter()
            .filter_map(|&k| std::env::var(k).ok().map(|v| (k.to_owned(), v)))
            .collect();
        Ok(Request { args, cwd, env, id })
    }
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(self.exit_code);
        put_bytes(&mut out, &self.stdout);
        put_bytes(&mut out, &self.stderr);
        out
    }

    pub fn decode(buf: &[u8]) -> io::Result<Response> {
        let mut at = 0;
        let exit_code = *buf.first().ok_or_else(truncated)?;
        at += 1;
        let stdout = get_bytes(buf, &mut at)?;
        let stderr = get_bytes(buf, &mut at)?;
        Ok(Response {
            exit_code,
            stdout,
            stderr,
        })
    }
}

/// Writes a length-prefixed frame to `w`.
pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// Reads one length-prefixed frame from `r`.
pub fn read_frame(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let req = Request {
            args: vec!["commit".into(), "-m".into(), "msg with spaces".into()],
            cwd: PathBuf::from("/some/work/dir"),
            env: vec![
                ("GIT_AUTHOR_NAME".into(), "tester".into()),
                ("ALT_RELAXED_DURABILITY".into(), "1".into()),
            ],
            id: Some([7u8; 16]),
        };
        assert_eq!(Request::decode(&req.encode()).unwrap(), req);
    }

    #[test]
    fn request_without_id_round_trips() {
        let req = Request {
            args: vec!["status".into()],
            cwd: PathBuf::from("/x"),
            env: Vec::new(),
            id: None,
        };
        assert_eq!(Request::decode(&req.encode()).unwrap(), req);
    }

    #[test]
    fn an_old_clients_frame_without_the_id_field_decodes_as_no_id() {
        // a pre-D5c frame ends after `env`, with no trailing id byte; the new
        // decoder must read it back as `id: None` (backward compatibility)
        let mut legacy = Vec::new();
        legacy.extend_from_slice(&1u32.to_le_bytes()); // 1 arg
        put_bytes(&mut legacy, b"status");
        put_bytes(&mut legacy, b"/x"); // cwd
        legacy.extend_from_slice(&0u32.to_le_bytes()); // 0 env pairs
        let req = Request::decode(&legacy).unwrap();
        assert_eq!(req.id, None);
        assert_eq!(req.args, vec!["status".to_string()]);
    }

    #[test]
    fn response_round_trips() {
        let resp = Response {
            exit_code: 1,
            stdout: b"on branch main\n".to_vec(),
            stderr: b"fatal: nope\n".to_vec(),
        };
        assert_eq!(Response::decode(&resp.encode()).unwrap(), resp);
    }

    #[test]
    fn empty_fields_round_trip() {
        let req = Request {
            args: Vec::new(),
            cwd: PathBuf::from(""),
            env: Vec::new(),
            id: None,
        };
        assert_eq!(Request::decode(&req.encode()).unwrap(), req);
        let resp = Response {
            exit_code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert_eq!(Response::decode(&resp.encode()).unwrap(), resp);
    }

    #[test]
    fn framing_round_trips_over_a_pipe() {
        let payload = Request {
            args: vec!["status".into(), "--json".into()],
            cwd: PathBuf::from("/x"),
            env: Vec::new(),
            id: None,
        }
        .encode();
        let mut buf = Vec::new();
        write_frame(&mut buf, &payload).unwrap();
        // extra trailing bytes must not be consumed by one read_frame
        buf.extend_from_slice(b"trailing");
        let mut cursor = std::io::Cursor::new(buf);
        let got = read_frame(&mut cursor).unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn truncated_frame_is_an_error() {
        assert!(Request::decode(&[0xff, 0xff, 0xff, 0xff]).is_err());
        assert!(Response::decode(&[]).is_err());
    }
}
