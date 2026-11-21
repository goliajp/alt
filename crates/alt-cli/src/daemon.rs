//! Local daemon wire protocol and framing.
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
    // structured principal: kind/id distinguish agent runs from human
    // logins, session correlates a multi-step agent run in the op log.
    "ALT_PRINCIPAL_KIND",
    "ALT_PRINCIPAL_ID",
    "ALT_SESSION_ID",
    // whom an agent acts for, recorded in commit metadata
    "ALT_CONTROLLING",
    "ALT_RELAXED_DURABILITY",
    "ALT_PROTECTED_OVERRIDE",
    "GIT_SEQUENCE_EDITOR",
    "GIT_EDITOR",
    "VISUAL",
    "EDITOR",
    "ALT_NO_CREDENTIAL_HELPER",
    // where the configuration outside the repository lies
    "HOME",
    "XDG_CONFIG_HOME",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_GLOBAL",
    // which character set texts are in
    "LC_ALL",
    "LC_CTYPE",
    "LANG",
];

/// Where the C library of this process looks for locales, as the daemon
/// writes it beside its socket: `LOCPATH` in hexadecimal, nothing when
/// it is not set. A locale is found by the path of the process that asks
/// for it, so a daemon answers only callers whose path is its own.
pub fn locale_path() -> String {
    let path = std::env::var_os("LOCPATH").unwrap_or_default();
    let bytes = path.as_encoded_bytes().iter();
    bytes.map(|byte| format!("{byte:02x}")).collect()
}

/// A client-chosen idempotency token: 16 bytes, unique per command invocation
/// and stable across that invocation's retries. The daemon stamps it on the
/// command's ref transaction and, on a same-id retry, detects the write as
/// already applied instead of running it twice (exactly-once). Structurally
/// identical to `alt_refs::IdemKey`.
pub type RequestId = [u8; 16];

/// A command to run: the alt argv (without the program name), the client's
/// working directory, the forwarded env vars, and an optional idempotency id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Exactly-once token for a non-idempotent write. `None` for reads,
    /// for the direct CLI path, and for an old client whose frame carries no
    /// trailing id field (the daemon then degrades to at-most-once).
    pub id: Option<RequestId>,
}

pub use alt_command_protocol::Response;

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

/// Reads an item count, rejecting one the rest of the frame cannot hold at
/// `min_item` bytes per item, so a hostile count cannot drive the allocation.
fn get_count(buf: &[u8], at: &mut usize, min_item: usize) -> io::Result<usize> {
    let n = get_u32(buf, at)? as usize;
    if n > (buf.len() - *at) / min_item {
        return Err(truncated());
    }
    Ok(n)
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
        let mut out = Vec::with_capacity(512);
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
        let n = get_count(buf, &mut at, 4)?;
        let mut args = Vec::with_capacity(n);
        for _ in 0..n {
            args.push(get_string(buf, &mut at)?);
        }
        let cwd = PathBuf::from(get_string(buf, &mut at)?);
        let m = get_count(buf, &mut at, 8)?;
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
}

/// The variables of [`FORWARDED_ENV`] this process has, in the order it
/// holds them; one whose value is not UTF-8 is left out.
pub fn forwarded_env() -> Vec<(String, String)> {
    let mut env = Vec::new();
    alt_os::env::each(|name, value| {
        let forwarded = FORWARDED_ENV.iter().find(|key| key.as_bytes() == name);
        if let (Some(key), Ok(value)) = (forwarded, std::str::from_utf8(value))
            && !env.iter().any(|(seen, _)| seen == key)
        {
            env.push(((*key).to_owned(), value.to_owned()));
        }
    });
    env
}

/// Writes a length-prefixed frame to `w`: the length and the payload in
/// one write where the writer takes both.
pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| bad("daemon frame exceeds the 32-bit length limit"))?
        .to_le_bytes();
    let written = w.write_vectored(&[io::IoSlice::new(&length), io::IoSlice::new(payload)])?;
    match written.checked_sub(length.len()) {
        Some(of_payload) => w.write_all(&payload[of_payload..])?,
        None => {
            w.write_all(&length[written..])?;
            w.write_all(payload)?;
        }
    }
    w.flush()
}

/// How much of a frame is given room before any of it has arrived.
const FIRST_ROOM: u64 = 64 << 10;

/// Reads one length-prefixed frame from `r`.
pub fn read_frame(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u64::from(u32::from_le_bytes(len));
    // a frame of ordinary size is read whole in one call; beyond that the
    // room grows with the bytes that actually arrive, so a length prefix
    // alone cannot make the peer allocate up to 4 GiB
    let mut payload = vec![0; len.min(FIRST_ROOM) as usize];
    r.read_exact(&mut payload)?;
    r.take(len - payload.len() as u64)
        .read_to_end(&mut payload)?;
    if payload.len() as u64 != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated daemon frame",
        ));
    }
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
    fn the_forwarded_variables_are_those_the_process_has_by_name() {
        let mut by_name: Vec<(String, String)> = FORWARDED_ENV
            .iter()
            .filter_map(|&key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
            .collect();
        let mut forwarded = forwarded_env();
        // a test run has a home and a user at least
        assert!(
            forwarded.iter().any(|(key, _)| key == "HOME"),
            "{forwarded:?}"
        );
        by_name.sort();
        forwarded.sort();
        assert_eq!(forwarded, by_name);
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
        // an old client's frame ends after `env`, with no trailing id byte; the new
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

    /// Takes at most `self.1` bytes of each write.
    struct Trickle(Vec<u8>, usize);

    impl Write for Trickle {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let taken = bytes.len().min(self.1);
            self.0.extend_from_slice(&bytes[..taken]);
            Ok(taken)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_frame_is_whole_however_little_each_write_takes() {
        let payload: Vec<u8> = (0..200u8).collect();
        for at_once in [1, 3, 4, 5, 150, 204, 1000] {
            let mut out = Trickle(Vec::new(), at_once);
            write_frame(&mut out, &payload).unwrap();
            assert_eq!(out.0[..4], 200u32.to_le_bytes(), "{at_once}");
            assert_eq!(read_frame(&mut io::Cursor::new(out.0)).unwrap(), payload);
        }
    }

    /// Counts the reads that reach it.
    struct Counted<R>(R, usize);

    impl<R: Read> Read for Counted<R> {
        fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
            self.1 += 1;
            self.0.read(into)
        }
    }

    #[test]
    fn a_frame_of_ordinary_size_is_read_in_two_calls() {
        for size in [0, 10, 31, 32, 33, 200, 5000, 60_000] {
            let payload = vec![7u8; size];
            let mut framed = Vec::new();
            write_frame(&mut framed, &payload).unwrap();
            let mut counted = Counted(io::Cursor::new(framed), 0);
            assert_eq!(read_frame(&mut counted).unwrap(), payload);
            assert!(counted.1 <= 2, "{size} bytes took {} reads", counted.1);
        }
    }

    #[test]
    fn frame_shorter_than_its_length_prefix_is_an_error() {
        let mut buf = u32::MAX.to_le_bytes().to_vec();
        buf.extend_from_slice(b"abc");
        let err = read_frame(&mut std::io::Cursor::new(buf)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn truncated_frame_is_an_error() {
        assert!(Request::decode(&[0xff, 0xff, 0xff, 0xff]).is_err());
        assert!(Response::decode(&[]).is_err());
    }

    #[test]
    fn oversized_env_count_is_an_error() {
        // zero args, empty cwd, then an env count no frame could back
        let mut frame = vec![0, 0, 0, 0, 0, 0, 0, 0];
        frame.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(Request::decode(&frame).is_err());
    }
}
