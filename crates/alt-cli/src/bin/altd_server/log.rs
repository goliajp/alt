//! One line of the access log for each request.

use super::*;

/// per-request log context filled in as dispatch progresses.
/// The outer serve loop reads it back after dispatch returns and emits
/// one JSON-line access log entry, so a single request is one line of
/// machine-readable observability — same style as `alt`'s `--json` paths.
#[derive(Default)]
pub(super) struct LogCtx {
    pub(super) status: u16,
    /// the request was handed back for its body and is not over
    pub(super) deferred: bool,
    pub(super) bytes_in: u64,
    /// response body byte count when the encoder knows the
    /// length up front (the common path — `Response::from_data`,
    /// `from_string`, and the pack/sideband encoders all hand over a
    /// finished `Vec<u8>`). `None` is rendered as JSON `null` in the
    /// access log.
    pub(super) bytes_out: Option<u64>,
    /// total fsync count from the process-wide
    /// `WriteCoordinator.group.fsync_count()` snapshot at response
    /// time. Set only on receive-pack responses (the path that
    /// actually contends for durability). When N concurrent pushes
    /// share a single group fsync, their access-log entries carry
    /// the same `fsync_seq` value — the coalescing signal.
    pub(super) fsync_seq: Option<u64>,
    pub(super) principal: Option<String>,
    pub(super) repo: Option<String>,
}

/// Monotonic short request id. A 12-char hex from an `AtomicU64` counter
/// keyed by the server's start time — enough to disambiguate within a
/// process lifetime without pulling in a uuid dep.
pub(super) fn next_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{n:012x}")
}

pub(super) fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Emit one JSON-line access log entry to stderr (the same channel
/// the bind line uses, so an operator's stderr capture sees both).
pub(super) fn emit_access_log(
    req_id: &str,
    method: &Method,
    url: &str,
    log: &LogCtx,
    duration_ms: u128,
) {
    use alt_cli::json::Json;
    let row = Json::Object(vec![
        ("ts_unix_ms", Json::Num(unix_ms_now() as i64)),
        ("req_id", Json::str(req_id)),
        ("method", Json::str(format!("{method:?}"))),
        ("path", Json::str(url)),
        ("status", Json::Num(i64::from(log.status))),
        ("duration_ms", Json::Num(duration_ms as i64)),
        ("bytes_in", Json::Num(log.bytes_in as i64)),
        (
            "bytes_out",
            match log.bytes_out {
                Some(n) => Json::Num(n as i64),
                None => Json::Null,
            },
        ),
        (
            "fsync_seq",
            match log.fsync_seq {
                Some(n) => Json::Num(n as i64),
                None => Json::Null,
            },
        ),
        (
            "principal",
            match &log.principal {
                Some(p) => Json::str(p.clone()),
                None => Json::Null,
            },
        ),
        (
            "repo",
            match &log.repo {
                Some(r) => Json::str(r.clone()),
                None => Json::Null,
            },
        ),
    ]);
    let mut buf = Vec::with_capacity(256);
    let _ = row.write(&mut buf);
    buf.push(b'\n');
    use std::io::Write;
    let _ = std::io::stderr().write_all(&buf);
}

/// Helper that captures the response's status code into the LogCtx and
/// then hands the request off to `respond`. Replaces every bare
/// `req.respond(resp)?` site so access logs always see the real status.
pub(super) fn respond_logged<R: std::io::Read + Send + 'static>(
    req: http::Request,
    resp: Response<R>,
    log: &mut LogCtx,
) -> std::io::Result<()> {
    log.status = resp.status_code().0;
    log.bytes_out = resp.data_length().map(|n| n as u64);
    req.respond(resp);
    Ok(())
}
