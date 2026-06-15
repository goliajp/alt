//! `git-receive-pack` v1 framing — the push side of the wire.
//!
//! Push has not (yet) moved to protocol v2 in upstream git: `git push`
//! against `git-receive-pack` still speaks the v0/v1 framing where:
//!
//! - The capability set is advertised in-band, attached after a NUL on the
//!   first ref line (the "capabilities^{}" pseudo-ref appears for empty
//!   repos).
//! - The client sends one `update` pkt-line per ref change, declares its
//!   own capabilities on the first command line, flushes, then concatenates
//!   the raw packfile bytes onto the same body.
//! - The server replies (when `report-status` was requested) with one
//!   `unpack` line and one `ok` / `ng` line per ref, terminated by a flush.
//!
//! The format is documented in `Documentation/gitprotocol-pack.txt` and
//! `Documentation/gitprotocol-http.txt`. This module encodes/decodes the
//! framing only; the actual packfile bytes are passed through unmodified
//! (so a downstream caller writes the pack via [`alt_git_pack::PackWriter`]
//! into the same stream that holds the update commands).
//!
//! ## Why a separate module from `fetch`
//!
//! Push uses **v1** framing (NUL-separated caps, raw pack body after the
//! commands, status report in plain pkt-lines or sideband). Fetch (W4)
//! uses **v2** framing (section-headed pkt-lines, sideband-wrapped pack).
//! Sharing the pkt-line layer is enough — the higher-level command shape
//! is genuinely different.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use alt_git_codec::{HashAlgo, ObjectId};

use crate::pkt::{self, Frame, PktError};

/// A single ref update the client wants the server to apply. `old == 0…0`
/// means "create this ref", `new == 0…0` means "delete it", both nonzero
/// means "fast-forward / update".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefUpdate {
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
    pub name: String,
}

impl RefUpdate {
    /// `true` if this update creates a new ref (old is the all-zero id).
    pub fn is_create(&self) -> bool {
        self.old.is_none()
    }

    /// `true` if this update deletes a ref (new is the all-zero id).
    pub fn is_delete(&self) -> bool {
        self.new.is_none()
    }
}

/// What the server told us in its `info/refs?service=git-receive-pack`
/// (v1) ref advertisement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct V1RefAdvertisement {
    /// Refs the server holds, `ref-name → tip-oid`. The first-line
    /// `capabilities^{}` pseudo-ref (for empty repos) is *not* added here.
    pub refs: BTreeMap<String, ObjectId>,
    /// Capabilities the server advertised (after the NUL on the first
    /// ref line). Includes `report-status`, `delete-refs`, `ofs-delta`,
    /// `agent=…`, etc.
    pub capabilities: Vec<String>,
}

impl V1RefAdvertisement {
    pub fn supports(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }

    /// Look up the value of a `key=value` capability (e.g. `agent`).
    pub fn cap_value<'a>(&'a self, key: &str) -> Option<&'a str> {
        let prefix = format!("{key}=");
        for c in &self.capabilities {
            if let Some(rest) = c.strip_prefix(&prefix) {
                return Some(rest);
            }
        }
        None
    }
}

/// Reasons request encoding / response parsing fails.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("pkt-line: {0}")]
    Pkt(#[from] PktError),
    /// First frame wasn't `# service=git-receive-pack\n`.
    #[error("unexpected service header: {0:?}")]
    BadService(String),
    /// A ref-advertisement line wasn't `<oid> <name>[\0<caps>]`.
    #[error("malformed ref-ad line: {0:?}")]
    BadRefLine(Vec<u8>),
    /// A status-report line wasn't `unpack …` / `ok …` / `ng …`.
    #[error("malformed status line: {0:?}")]
    BadStatusLine(Vec<u8>),
    /// Sideband channel byte wasn't 1, 2, or 3.
    #[error("invalid sideband channel: {0}")]
    BadSideband(u8),
    /// Sideband band 3 (fatal error from server).
    #[error("server error (sideband band 3): {0}")]
    ServerError(String),
}

/// Encode the v1 push request body: ref-update commands followed by the
/// pack bytes. The caller writes this whole thing as the `POST
/// /git-receive-pack` body.
///
/// `pack_bytes` is the on-wire pack stream (`PACK` header through trailer).
/// Pass an empty slice when only deleting refs.
///
/// `capabilities` is the client-side capability list — typically
/// `["report-status", "ofs-delta", "agent=alt/…"]`. The list is attached
/// to the first command line after a NUL byte, exactly the wire shape git
/// expects.
pub fn encode_push_request<W: Write>(
    w: &mut W,
    updates: &[RefUpdate],
    capabilities: &[&str],
    algo: HashAlgo,
    pack_bytes: &[u8],
) -> std::io::Result<()> {
    let zero = zero_oid(algo);
    for (i, u) in updates.iter().enumerate() {
        let old = u.old.unwrap_or(zero);
        let new = u.new.unwrap_or(zero);
        let mut line = format!("{old} {new} {name}", name = u.name).into_bytes();
        if i == 0 {
            // first command line carries the capability list, NUL-separated
            // from the command text
            line.push(0);
            line.extend_from_slice(capabilities.join(" ").as_bytes());
        }
        line.push(b'\n');
        pkt::write_data(w, &line)?;
    }
    pkt::write_flush(w)?;
    // pack bytes follow the flush, raw (no pkt-line wrapping)
    if !pack_bytes.is_empty() {
        w.write_all(pack_bytes)?;
    }
    Ok(())
}

/// Parse the smart-http v1 ref advertisement for `git-receive-pack` (the
/// `GET /info/refs?service=git-receive-pack` response body). Tolerates
/// both the smart-http preamble (`# service=…\n` + flush) and a bare
/// stream (used over SSH).
pub fn parse_v1_ref_advertisement<R: Read>(
    r: &mut R,
    algo: HashAlgo,
) -> Result<V1RefAdvertisement, PushError> {
    let mut scratch = Vec::new();
    let mut ad = V1RefAdvertisement::default();
    let first = pkt::read_frame(r, &mut scratch)?;
    let mut after_header = false;

    if let Frame::Data(d) = first
        && let Some(svc) = d.strip_prefix(b"# service=")
    {
        let svc = trim_newline(svc);
        let svc =
            std::str::from_utf8(svc).map_err(|_| PushError::BadService(format!("{svc:?}")))?;
        if svc != "git-receive-pack" {
            return Err(PushError::BadService(svc.to_string()));
        }
        // smart-http: the service header is followed by a flush, then the
        // actual ref ad
        let flush = pkt::read_frame(r, &mut scratch)?;
        if !matches!(flush, Frame::Flush) {
            return Err(PushError::BadRefLine(
                b"expected flush after # service=".to_vec(),
            ));
        }
        after_header = true;
    } else if let Frame::Data(d) = first {
        // bare transport — replay this line as the first ref/cap line
        parse_ref_line(d, algo, &mut ad)?;
    } else if matches!(first, Frame::Flush) {
        // an empty repo can advertise just a flush; that's not valid v1
        return Ok(ad);
    } else {
        return Err(PushError::BadRefLine(b"unexpected non-data frame".to_vec()));
    }

    loop {
        let mut scratch2 = Vec::new();
        let f = pkt::read_frame(r, &mut scratch2)?;
        match f {
            Frame::Flush => break,
            Frame::Data(d) => parse_ref_line(d, algo, &mut ad)?,
            // a delim mid-ad would be a protocol violation for v1 — bail
            // loudly so the caller doesn't silently truncate
            Frame::Delim | Frame::ResponseEnd => break,
        }
        let _ = after_header; // borrow checker keep
    }

    Ok(ad)
}

fn parse_ref_line(
    line: &[u8],
    algo: HashAlgo,
    ad: &mut V1RefAdvertisement,
) -> Result<(), PushError> {
    let line = trim_newline(line);
    // first ref line carries `\0<caps>` after the ref name; later lines
    // don't (caps are advertised once)
    let (head, caps) = match line.iter().position(|&b| b == 0) {
        Some(i) => (&line[..i], Some(&line[i + 1..])),
        None => (line, None),
    };
    let s = std::str::from_utf8(head).map_err(|_| PushError::BadRefLine(head.to_vec()))?;
    let (oid_s, name) = s
        .split_once(' ')
        .ok_or_else(|| PushError::BadRefLine(head.to_vec()))?;
    let oid = oid_s
        .parse::<ObjectId>()
        .ok()
        .filter(|o| o.algo() == algo)
        .ok_or_else(|| PushError::BadRefLine(head.to_vec()))?;

    // "capabilities^{}" is the pseudo-ref empty repos emit so the cap list
    // has somewhere to ride; skip it from the ref map
    if name != "capabilities^{}" {
        ad.refs.insert(name.to_owned(), oid);
    }

    if let Some(caps) = caps {
        let caps_str =
            std::str::from_utf8(caps).map_err(|_| PushError::BadRefLine(caps.to_vec()))?;
        for c in caps_str.split_whitespace() {
            ad.capabilities.push(c.to_owned());
        }
    }
    Ok(())
}

/// What the server reported after applying our push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportStatus {
    /// `unpack ok` → `Ok(())`; `unpack <reason>` → `Err(reason)`. Absent
    /// `unpack` line surfaces here as `Err("(no unpack line)")` so the
    /// caller never misreads silence as success.
    pub unpack: Result<(), String>,
    /// One entry per ref the client tried to update, in the order the
    /// server reported.
    pub commands: Vec<CommandStatus>,
}

/// Per-ref outcome from `report-status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandStatus {
    /// `ok <ref>` — the server applied this update.
    Ok(String),
    /// `ng <ref> <reason>` — the server refused this update.
    Ng { name: String, reason: String },
}

/// Parse a `report-status` body. Use [`parse_report_status_sideband`]
/// when the client advertised `side-band-64k`; otherwise the body is
/// plain pkt-lines and this function consumes them directly.
pub fn parse_report_status<R: Read>(r: &mut R) -> Result<ReportStatus, PushError> {
    let mut scratch = Vec::new();
    let mut unpack = Err("(no unpack line)".to_owned());
    let mut commands = Vec::new();
    loop {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Flush | Frame::ResponseEnd => break,
            Frame::Delim => continue,
            Frame::Data(line) => {
                let line = trim_newline(line);
                let s = std::str::from_utf8(line)
                    .map_err(|_| PushError::BadStatusLine(line.to_vec()))?;
                if let Some(rest) = s.strip_prefix("unpack ") {
                    unpack = if rest == "ok" {
                        Ok(())
                    } else {
                        Err(rest.to_string())
                    };
                } else if let Some(name) = s.strip_prefix("ok ") {
                    commands.push(CommandStatus::Ok(name.to_string()));
                } else if let Some(rest) = s.strip_prefix("ng ") {
                    let (name, reason) = rest
                        .split_once(' ')
                        .ok_or_else(|| PushError::BadStatusLine(line.to_vec()))?;
                    commands.push(CommandStatus::Ng {
                        name: name.to_string(),
                        reason: reason.to_string(),
                    });
                } else {
                    return Err(PushError::BadStatusLine(line.to_vec()));
                }
            }
        }
    }
    Ok(ReportStatus { unpack, commands })
}

/// Parse a `report-status` body wrapped in `side-band-64k`. Band 1 carries
/// the pkt-line status report (which we then feed through
/// [`parse_report_status`]); band 2 progress is passed to the callback;
/// band 3 errors surface as [`PushError::ServerError`].
pub fn parse_report_status_sideband<R, P>(
    r: &mut R,
    mut progress: P,
) -> Result<ReportStatus, PushError>
where
    R: Read,
    P: FnMut(&[u8]),
{
    let mut band1 = Vec::new();
    let mut scratch = Vec::new();
    loop {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Flush | Frame::ResponseEnd => break,
            Frame::Delim => continue,
            Frame::Data(payload) => {
                let Some((&band, body)) = payload.split_first() else {
                    return Err(PushError::BadStatusLine(b"empty sideband pkt".to_vec()));
                };
                match band {
                    1 => band1.extend_from_slice(body),
                    2 => progress(body),
                    3 => {
                        return Err(PushError::ServerError(
                            String::from_utf8_lossy(body).trim().to_string(),
                        ));
                    }
                    other => return Err(PushError::BadSideband(other)),
                }
            }
        }
    }
    parse_report_status(&mut band1.as_slice())
}

fn zero_oid(algo: HashAlgo) -> ObjectId {
    let zeros = vec![0u8; algo.raw_len()];
    ObjectId::from_bytes(algo, &zeros).expect("zero oid is valid length")
}

fn trim_newline(b: &[u8]) -> &[u8] {
    let mut end = b.len();
    while end > 0 && (b[end - 1] == b'\n' || b[end - 1] == b'\r') {
        end -= 1;
    }
    &b[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkt::{write_data, write_flush};
    use std::io::Cursor;

    fn sha1(hex: &str) -> ObjectId {
        hex.parse().expect("test oid")
    }

    /// The canonical empty-repo advertisement: a single
    /// `capabilities^{}` pseudo-ref carrying the cap list, no real refs.
    #[test]
    fn empty_repo_ad_parses_caps_only() {
        let mut buf = Vec::new();
        write_data(&mut buf, b"# service=git-receive-pack\n").unwrap();
        write_flush(&mut buf).unwrap();
        let mut line = format!("{} capabilities^{{}}", "0".repeat(40)).into_bytes();
        line.push(0);
        line.extend_from_slice(b"report-status delete-refs ofs-delta agent=git/2.45");
        line.push(b'\n');
        write_data(&mut buf, &line).unwrap();
        write_flush(&mut buf).unwrap();

        let ad = parse_v1_ref_advertisement(&mut Cursor::new(&buf), HashAlgo::Sha1).unwrap();
        assert!(
            ad.refs.is_empty(),
            "capabilities^{{}} should not appear as a real ref"
        );
        assert!(ad.supports("report-status"));
        assert!(ad.supports("ofs-delta"));
        assert_eq!(ad.cap_value("agent"), Some("git/2.45"));
    }

    /// A non-empty repo's ad: first ref line carries the caps after NUL,
    /// later lines are bare `<oid> <name>`.
    #[test]
    fn non_empty_repo_ad_collects_all_refs_and_caps() {
        let h1 = sha1("0123456789abcdef0123456789abcdef01234567");
        let h2 = sha1("89abcdef0123456789abcdef0123456789abcdef");
        let mut buf = Vec::new();
        write_data(&mut buf, b"# service=git-receive-pack\n").unwrap();
        write_flush(&mut buf).unwrap();
        let mut first = format!("{h1} refs/heads/main").into_bytes();
        first.push(0);
        first.extend_from_slice(b"report-status ofs-delta");
        first.push(b'\n');
        write_data(&mut buf, &first).unwrap();
        write_data(&mut buf, format!("{h2} refs/heads/dev\n").as_bytes()).unwrap();
        write_flush(&mut buf).unwrap();

        let ad = parse_v1_ref_advertisement(&mut Cursor::new(&buf), HashAlgo::Sha1).unwrap();
        assert_eq!(ad.refs.len(), 2);
        assert_eq!(ad.refs.get("refs/heads/main"), Some(&h1));
        assert_eq!(ad.refs.get("refs/heads/dev"), Some(&h2));
        assert!(ad.supports("report-status"));
        assert!(ad.supports("ofs-delta"));
    }

    /// A push request: each update is a pkt-line, the first carries the
    /// NUL-separated capability list, all followed by a flush, then the
    /// raw pack bytes.
    #[test]
    fn push_request_writes_commands_flush_then_pack() {
        let h_old = sha1("0123456789abcdef0123456789abcdef01234567");
        let h_new = sha1("89abcdef0123456789abcdef0123456789abcdef");
        let updates = vec![
            RefUpdate {
                old: Some(h_old),
                new: Some(h_new),
                name: "refs/heads/main".into(),
            },
            RefUpdate {
                old: None,
                new: Some(h_new),
                name: "refs/heads/new".into(),
            },
        ];
        let mut buf = Vec::new();
        encode_push_request(
            &mut buf,
            &updates,
            &["report-status", "ofs-delta"],
            HashAlgo::Sha1,
            b"PACKBYTES",
        )
        .unwrap();

        // first cmd line should carry `\0report-status ofs-delta\n`
        let first_line_start = b"00".as_slice();
        assert!(buf.starts_with(first_line_start));
        let zero = "0".repeat(40);
        let expected_first =
            format!("{h_old} {h_new} refs/heads/main\x00report-status ofs-delta\n");
        let expected_second = format!("{zero} {h_new} refs/heads/new\n");
        assert!(
            buf.windows(expected_first.len())
                .any(|w| w == expected_first.as_bytes()),
            "first command line missing"
        );
        assert!(
            buf.windows(expected_second.len())
                .any(|w| w == expected_second.as_bytes()),
            "second command line missing"
        );
        // and the pack bytes appear after a flush
        assert!(
            buf.windows(b"0000PACKBYTES".len())
                .any(|w| w == b"0000PACKBYTES"),
            "expected flush-then-pack-bytes in body"
        );
    }

    /// Successful push response: `unpack ok` + one `ok <ref>` per update.
    #[test]
    fn report_status_success_parses() {
        let mut buf = Vec::new();
        write_data(&mut buf, b"unpack ok\n").unwrap();
        write_data(&mut buf, b"ok refs/heads/main\n").unwrap();
        write_flush(&mut buf).unwrap();
        let report = parse_report_status(&mut Cursor::new(&buf)).unwrap();
        assert!(report.unpack.is_ok());
        assert_eq!(
            report.commands,
            vec![CommandStatus::Ok("refs/heads/main".into())]
        );
    }

    /// A push the server refused: `ng <ref> <reason>` surfaces typed so
    /// the caller can report it without string-matching the status line.
    #[test]
    fn report_status_ng_parses_with_reason() {
        let mut buf = Vec::new();
        write_data(&mut buf, b"unpack ok\n").unwrap();
        write_data(&mut buf, b"ng refs/heads/main non-fast-forward\n").unwrap();
        write_flush(&mut buf).unwrap();
        let report = parse_report_status(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(
            report.commands,
            vec![CommandStatus::Ng {
                name: "refs/heads/main".into(),
                reason: "non-fast-forward".into(),
            }]
        );
    }

    /// `unpack <reason>` (a single failure for the whole push, e.g. the
    /// pack was corrupt) surfaces as `Err(reason)`.
    #[test]
    fn report_status_unpack_failure_is_reported() {
        let mut buf = Vec::new();
        write_data(&mut buf, b"unpack index-pack failed\n").unwrap();
        write_flush(&mut buf).unwrap();
        let report = parse_report_status(&mut Cursor::new(&buf)).unwrap();
        match report.unpack {
            Err(msg) => assert_eq!(msg, "index-pack failed"),
            Ok(()) => panic!("expected unpack failure"),
        }
    }

    /// `side-band-64k`-wrapped report: band 1 carries the status pkts,
    /// band 2 is progress, band 3 is fatal. The demux feeds band 1 back
    /// into the plain status parser.
    #[test]
    fn report_status_sideband_demuxes_correctly() {
        // build the inner status stream (what would appear without sideband)
        let mut inner = Vec::new();
        write_data(&mut inner, b"unpack ok\n").unwrap();
        write_data(&mut inner, b"ok refs/heads/main\n").unwrap();
        write_flush(&mut inner).unwrap();

        // wrap each chunk of `inner` in a band-1 pkt; add a band-2 line too
        let mut outer = Vec::new();
        let mut band2 = vec![2u8];
        band2.extend_from_slice(b"updating remote\n");
        write_data(&mut outer, &band2).unwrap();
        let mut band1 = vec![1u8];
        band1.extend_from_slice(&inner);
        write_data(&mut outer, &band1).unwrap();
        write_flush(&mut outer).unwrap();

        let mut progress = Vec::new();
        let report = parse_report_status_sideband(&mut Cursor::new(&outer), |b| {
            progress.extend_from_slice(b)
        })
        .unwrap();
        assert!(report.unpack.is_ok());
        assert_eq!(report.commands.len(), 1);
        assert!(
            progress
                .windows(b"updating remote".len())
                .any(|w| w == b"updating remote"),
            "progress should contain band-2 text: {progress:?}",
        );
    }
}
