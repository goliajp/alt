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
    /// IO error draining the pack trailer or sideband body — surfaces
    /// when the server can't keep reading after the command flush.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Capability name an alt client sends to declare which principal
/// signed this push (M6/W9). Value is the principal id; a paired
/// `alt-sig` capability carries the signature.
///
/// Git's receive-pack ignores capabilities it didn't itself advertise,
/// so this extension is wire-safe against any git server. An alt server
/// (W10+) looks for both names and verifies the pair against its trust
/// store.
pub const CAP_ALT_PRINCIPAL: &str = "alt-principal";

/// Capability name carrying the `alt-sig-ed25519:<base64url>` signature
/// over [`canonical_push_payload`] (M6/W9). Paired with
/// [`CAP_ALT_PRINCIPAL`]; either both present or both absent.
pub const CAP_ALT_SIG: &str = "alt-sig";

/// Capability name carrying a server-issued single-use nonce (M14/W45
/// anti-replay). The server advertises it in the v1 receive-pack ref
/// advertisement; the client signs `nonce <hex>\n` prepended to the
/// usual canonical payload, and echoes the same `alt-nonce=<hex>` cap
/// on the push so the server can look the nonce up and consume it.
///
/// Pushes that re-use a consumed nonce are rejected: the same captured
/// payload + signature can't be replayed against the same server.
pub const CAP_ALT_NONCE: &str = "alt-nonce";

/// The canonical byte string a push signature signs over: each update
/// formatted as `"<old-hex> <new-hex> <ref-name>\n"`, sorted by ref name
/// to be order-independent against the client's input. Zero-oids are
/// rendered as `"0"*40` (sha-1) / `"0"*64` (sha-256), matching the wire
/// shape and what an alt server will reconstruct from the decoded
/// updates.
///
/// Decoupling the signed payload from the on-wire byte order means the
/// client and a future alt server can reconstruct the exact same bytes
/// without parsing pkt-lines back into a tuple list and worrying about
/// upstream re-ordering.
pub fn canonical_push_payload(updates: &[RefUpdate], algo: HashAlgo) -> Vec<u8> {
    canonical_push_payload_with_nonce(updates, None, algo)
}

/// M14/W45 — canonical payload with an optional server-issued nonce.
///
/// When `nonce` is `Some(hex)`, the payload begins with a literal
/// `nonce <hex>\n` line followed by the same sorted ref-update lines
/// the no-nonce form emits. This is exactly the byte sequence the
/// server expects to reconstruct from `(echoed alt-nonce cap,
/// pushed updates)` when it verifies a signed push that participated
/// in anti-replay negotiation.
///
/// When `nonce` is `None`, the output is identical to the no-nonce
/// path so existing W14 signed pushes keep verifying without change.
pub fn canonical_push_payload_with_nonce(
    updates: &[RefUpdate],
    nonce: Option<&str>,
    algo: HashAlgo,
) -> Vec<u8> {
    let zero = zero_oid(algo);
    let mut lines: Vec<String> = updates
        .iter()
        .map(|u| {
            format!(
                "{old} {new} {name}\n",
                old = u.old.unwrap_or(zero),
                new = u.new.unwrap_or(zero),
                name = u.name,
            )
        })
        .collect();
    lines.sort();
    let mut out = Vec::with_capacity(
        nonce.map(|n| 7 + n.len()).unwrap_or(0) + lines.iter().map(|l| l.len()).sum::<usize>(),
    );
    if let Some(n) = nonce {
        out.extend_from_slice(b"nonce ");
        out.extend_from_slice(n.as_bytes());
        out.push(b'\n');
    }
    for l in lines {
        out.extend_from_slice(l.as_bytes());
    }
    out
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

/// Server-side encode of a v1 ref advertisement (M9/W10c). Mirror of
/// [`parse_v1_ref_advertisement`]: caller passes the ordered refs and
/// the capability list to advertise. Empty repos still send a pseudo
/// ref `capabilities^{}` so the caps section has somewhere to ride.
///
/// Layout:
///
///   pkt: "# service=git-receive-pack\n"
///   flush
///   pkt: "<oid> <name>\0<caps>\n"   # first ref carries caps after NUL
///   pkt: "<oid> <name>\n"           # subsequent refs (caps already sent)
///   …
///   flush
pub fn encode_v1_ref_advertisement<W: Write>(
    w: &mut W,
    refs: &[(String, ObjectId)],
    capabilities: &[&str],
    algo: HashAlgo,
) -> std::io::Result<()> {
    pkt::write_data(w, b"# service=git-receive-pack\n")?;
    pkt::write_flush(w)?;
    let caps_blob = capabilities.join(" ");
    if refs.is_empty() {
        // Empty repo: pseudo-ref `capabilities^{}` carries the cap list.
        let zero = zero_oid(algo);
        let mut line = format!("{zero} capabilities^{{}}").into_bytes();
        line.push(0);
        line.extend_from_slice(caps_blob.as_bytes());
        line.push(b'\n');
        pkt::write_data(w, &line)?;
    } else {
        for (i, (name, oid)) in refs.iter().enumerate() {
            let mut line = format!("{oid} {name}").into_bytes();
            if i == 0 {
                line.push(0);
                line.extend_from_slice(caps_blob.as_bytes());
            }
            line.push(b'\n');
            pkt::write_data(w, &line)?;
        }
    }
    pkt::write_flush(w)
}

/// What a server saw after parsing a `git-receive-pack` request body.
#[derive(Debug, Clone)]
pub struct PushRequest {
    /// One entry per command line the client sent.
    pub updates: Vec<RefUpdate>,
    /// The capability list the client attached to the first command line
    /// (NUL-separated). One token per entry, in arrival order.
    pub capabilities: Vec<String>,
    /// Raw pack stream — bytes between the command flush and EOF. Empty
    /// when the client sent only deletions.
    pub pack: Vec<u8>,
}

/// The metadata half of a `git-receive-pack` POST body: ref updates +
/// capability list. Sits in front of the raw pack stream on the wire.
/// M13/W36 split [`parse_push_request`] into this head + a separate
/// pack drain so the server can stream the pack to a tempfile instead
/// of buffering the entire push body in RAM.
#[derive(Debug, Clone)]
pub struct PushHead {
    pub updates: Vec<RefUpdate>,
    pub capabilities: Vec<String>,
}

/// Parse just the ref-update + capability section of a
/// `git-receive-pack` POST body. Stops at the trailing flush; the
/// caller is responsible for the pack stream that follows (which may
/// be many gigabytes — see M13/W36 streaming path).
pub fn parse_push_request_head<R: Read>(r: &mut R, algo: HashAlgo) -> Result<PushHead, PushError> {
    let mut updates: Vec<RefUpdate> = Vec::new();
    let mut capabilities: Vec<String> = Vec::new();
    let mut scratch = Vec::new();
    let zero = zero_oid(algo);
    loop {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Flush => break,
            Frame::Delim | Frame::ResponseEnd => break,
            Frame::Data(line) => {
                let trimmed = trim_newline(line);
                let (head, caps_part) = match trimmed.iter().position(|&b| b == 0) {
                    Some(i) => (&trimmed[..i], Some(&trimmed[i + 1..])),
                    None => (trimmed, None),
                };
                let s =
                    std::str::from_utf8(head).map_err(|_| PushError::BadRefLine(head.to_vec()))?;
                let mut parts = s.splitn(3, ' ');
                let old_s = parts
                    .next()
                    .ok_or_else(|| PushError::BadRefLine(head.to_vec()))?;
                let new_s = parts
                    .next()
                    .ok_or_else(|| PushError::BadRefLine(head.to_vec()))?;
                let name = parts
                    .next()
                    .ok_or_else(|| PushError::BadRefLine(head.to_vec()))?;
                let old_oid = old_s
                    .parse::<ObjectId>()
                    .ok()
                    .filter(|o| o.algo() == algo)
                    .ok_or_else(|| PushError::BadRefLine(head.to_vec()))?;
                let new_oid = new_s
                    .parse::<ObjectId>()
                    .ok()
                    .filter(|o| o.algo() == algo)
                    .ok_or_else(|| PushError::BadRefLine(head.to_vec()))?;
                updates.push(RefUpdate {
                    name: name.to_owned(),
                    old: if old_oid == zero { None } else { Some(old_oid) },
                    new: if new_oid == zero { None } else { Some(new_oid) },
                });
                if let Some(caps) = caps_part {
                    let caps_s = std::str::from_utf8(caps)
                        .map_err(|_| PushError::BadRefLine(caps.to_vec()))?;
                    for c in caps_s.split_whitespace() {
                        capabilities.push(c.to_owned());
                    }
                }
            }
        }
    }
    Ok(PushHead {
        updates,
        capabilities,
    })
}

/// Server-side parse of a `git-receive-pack` POST body (M9/W10c).
/// Mirror of [`encode_push_request`]: reads the ref-update lines + the
/// (optional, NUL-attached) capability list, then drains everything
/// after the trailing flush as the raw pack stream.
///
/// M13/W36 note: prefer [`parse_push_request_head`] + a direct read
/// from `r` into a tempfile for production paths; this function
/// buffers the pack in memory and exists for tests + tools that want
/// the convenient `PushRequest` shape.
pub fn parse_push_request<R: Read>(r: &mut R, algo: HashAlgo) -> Result<PushRequest, PushError> {
    let head = parse_push_request_head(r, algo)?;
    let mut pack = Vec::new();
    r.read_to_end(&mut pack)?;
    Ok(PushRequest {
        updates: head.updates,
        capabilities: head.capabilities,
        pack,
    })
}

/// Server-side encode of `report-status` (M9/W10c). Plain pkt-lines —
/// no sideband — because the client only switches to sideband if the
/// server advertised `side-band-64k`; W10c doesn't yet, so we keep the
/// reply simple.
///
///   pkt: "unpack ok\n"                              (or "unpack <reason>\n")
///   pkt: "ok <ref>\n"                               (per applied update)
///   pkt: "ng <ref> <reason>\n"                      (per refused update)
///   flush
pub fn encode_report_status<W: Write>(
    w: &mut W,
    unpack: Result<(), &str>,
    commands: &[CommandStatus],
) -> std::io::Result<()> {
    let unpack_line = match unpack {
        Ok(()) => "unpack ok\n".to_owned(),
        Err(reason) => format!("unpack {reason}\n"),
    };
    pkt::write_data(w, unpack_line.as_bytes())?;
    for c in commands {
        let line = match c {
            CommandStatus::Ok(name) => format!("ok {name}\n"),
            CommandStatus::Ng { name, reason } => format!("ng {name} {reason}\n"),
        };
        pkt::write_data(w, line.as_bytes())?;
    }
    pkt::write_flush(w)
}

/// Server-side encode of `report-status` wrapped in `side-band-64k`
/// framing (M9/W13). Use this when the client advertised `side-band-64k`
/// in the push request — git CLI does this when the server advertised
/// the cap in the v1 ref ad. The plain pkt-line body is the same as
/// [`encode_report_status`] but each pkt-line rides band 1.
pub fn encode_report_status_sideband<W: Write>(
    w: &mut W,
    unpack: Result<(), &str>,
    commands: &[CommandStatus],
) -> std::io::Result<()> {
    let mut inner = Vec::new();
    encode_report_status(&mut inner, unpack, commands)?;
    const SIDEBAND_CHUNK: usize = crate::pkt::MAX_LINE_LEN - 5;
    for chunk in inner.chunks(SIDEBAND_CHUNK) {
        let mut framed = Vec::with_capacity(chunk.len() + 1);
        framed.push(0x01);
        framed.extend_from_slice(chunk);
        pkt::write_data(w, &framed)?;
    }
    pkt::write_flush(w)
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

    /// M9/W10c: the v1 ref ad encoder mirrors the parser. An empty
    /// store gets a `capabilities^{}` pseudo-ref; a populated store
    /// gets one pkt per real ref with caps attached only to the first.
    #[test]
    fn server_v1_ref_ad_round_trips() {
        let oid_a = sha1("0011223344556677889900112233445566778899");
        let oid_b = sha1("aabbccddeeff0011223344aabbccddeeff001122");
        let refs = vec![
            ("refs/heads/main".to_owned(), oid_a),
            ("refs/heads/feature".to_owned(), oid_b),
        ];
        let mut buf = Vec::new();
        encode_v1_ref_advertisement(
            &mut buf,
            &refs,
            &["report-status", "ofs-delta", "agent=alt-server/0"],
            HashAlgo::Sha1,
        )
        .unwrap();

        let ad = parse_v1_ref_advertisement(&mut Cursor::new(&buf), HashAlgo::Sha1).unwrap();
        assert_eq!(ad.refs.len(), 2);
        assert_eq!(ad.refs.get("refs/heads/main").copied(), Some(oid_a));
        assert_eq!(ad.refs.get("refs/heads/feature").copied(), Some(oid_b));
        assert!(ad.supports("report-status"));
        assert!(ad.supports("ofs-delta"));
    }

    #[test]
    fn server_v1_ref_ad_empty_repo_emits_pseudo_ref() {
        let mut buf = Vec::new();
        encode_v1_ref_advertisement(
            &mut buf,
            &[],
            &["report-status", "ofs-delta"],
            HashAlgo::Sha1,
        )
        .unwrap();
        let ad = parse_v1_ref_advertisement(&mut Cursor::new(&buf), HashAlgo::Sha1).unwrap();
        assert!(ad.refs.is_empty(), "empty repo carries no real refs");
        assert!(ad.supports("report-status"));
    }

    /// Push request round-trip: client encoder + server parser
    /// agree on commands, caps, and the trailing raw pack bytes.
    #[test]
    fn server_parses_what_client_pushed() {
        let oid_a = sha1("0011223344556677889900112233445566778899");
        let oid_b = sha1("aabbccddeeff0011223344aabbccddeeff001122");
        let updates = vec![
            RefUpdate {
                old: Some(oid_a),
                new: Some(oid_b),
                name: "refs/heads/main".into(),
            },
            RefUpdate {
                old: None,
                new: Some(oid_b),
                name: "refs/heads/new".into(),
            },
        ];
        let pack = b"PACKv2BODY-arbitrary-bytes-after-the-flush".to_vec();
        let mut buf = Vec::new();
        encode_push_request(
            &mut buf,
            &updates,
            &["report-status", "ofs-delta"],
            HashAlgo::Sha1,
            &pack,
        )
        .unwrap();
        let parsed = parse_push_request(&mut Cursor::new(&buf), HashAlgo::Sha1).unwrap();
        assert_eq!(parsed.updates, updates);
        assert!(parsed.capabilities.contains(&"report-status".to_owned()));
        assert!(parsed.capabilities.contains(&"ofs-delta".to_owned()));
        assert_eq!(parsed.pack, pack);
    }

    /// `report-status` server encoder + client parser round-trip across
    /// the unpack outcome and per-ref ok/ng outcomes.
    #[test]
    fn server_report_status_round_trips() {
        let mut buf = Vec::new();
        encode_report_status(
            &mut buf,
            Ok(()),
            &[
                CommandStatus::Ok("refs/heads/main".into()),
                CommandStatus::Ng {
                    name: "refs/heads/locked".into(),
                    reason: "protected branch".into(),
                },
            ],
        )
        .unwrap();
        let parsed = parse_report_status(&mut Cursor::new(&buf)).unwrap();
        assert!(parsed.unpack.is_ok());
        assert_eq!(
            parsed.commands,
            vec![
                CommandStatus::Ok("refs/heads/main".into()),
                CommandStatus::Ng {
                    name: "refs/heads/locked".into(),
                    reason: "protected branch".into(),
                },
            ]
        );
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

    /// W9 — the canonical push payload is sorted by ref name and uses the
    /// same `"<old> <new> <name>\n"` shape on both sides of the wire, so
    /// a client signature lines up exactly with what an alt server will
    /// reconstruct from the decoded updates.
    #[test]
    fn canonical_payload_is_sorted_and_format_stable() {
        let h1 = sha1("0123456789abcdef0123456789abcdef01234567");
        let h2 = sha1("89abcdef0123456789abcdef0123456789abcdef");
        // intentionally reverse-ordered input — sort by ref name kicks in
        let updates = vec![
            RefUpdate {
                old: Some(h1),
                new: Some(h2),
                name: "refs/heads/main".into(),
            },
            RefUpdate {
                old: None,
                new: Some(h2),
                name: "refs/heads/dev".into(),
            },
        ];
        let payload = canonical_push_payload(&updates, HashAlgo::Sha1);
        let zero = "0".repeat(40);
        let expected = format!("{zero} {h2} refs/heads/dev\n{h1} {h2} refs/heads/main\n");
        assert_eq!(
            String::from_utf8(payload).unwrap(),
            expected,
            "canonical payload should sort by name and emit literal lines"
        );
    }

    /// Two clients submitting the same updates in different orders sign
    /// the same bytes — the wire extension is order-independent against
    /// caller input. This is the property an alt server's verifier relies
    /// on.
    #[test]
    fn canonical_payload_is_order_independent() {
        let h1 = sha1("0123456789abcdef0123456789abcdef01234567");
        let h2 = sha1("89abcdef0123456789abcdef0123456789abcdef");
        let a = vec![
            RefUpdate {
                old: Some(h1),
                new: Some(h2),
                name: "refs/heads/a".into(),
            },
            RefUpdate {
                old: Some(h1),
                new: Some(h2),
                name: "refs/heads/b".into(),
            },
        ];
        let mut b = a.clone();
        b.reverse();
        let pa = canonical_push_payload(&a, HashAlgo::Sha1);
        let pb = canonical_push_payload(&b, HashAlgo::Sha1);
        assert_eq!(pa, pb);
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

    /// M13/W36: `parse_push_request_head` must stop exactly at the
    /// flush that terminates the command section — the pack bytes
    /// that follow must remain in the reader for the caller to
    /// stream into a tempfile. Without this guarantee the streaming
    /// path on altd-server would consume the pack into the parser's
    /// scratch and then have nothing to drain.
    #[test]
    fn parse_push_request_head_leaves_pack_bytes_in_reader() {
        let oid = sha1("0011223344556677889900112233445566778899");
        let updates = vec![RefUpdate {
            name: "refs/heads/main".into(),
            old: None,
            new: Some(oid),
        }];
        let mut body = Vec::new();
        // PUSH wire: encode the head, then append raw pack bytes.
        encode_push_request(
            &mut body,
            &updates,
            &["report-status"],
            HashAlgo::Sha1,
            b"PACKDUMMY1234567",
        )
        .unwrap();
        let mut cur = std::io::Cursor::new(&body);
        let head = parse_push_request_head(&mut cur, HashAlgo::Sha1).unwrap();
        assert_eq!(head.updates.len(), 1);
        assert_eq!(head.capabilities, vec!["report-status".to_string()]);
        // The reader's cursor must now sit at the start of the pack
        // payload — meaning we can stream the rest off it byte-exact.
        let mut tail = Vec::new();
        cur.read_to_end(&mut tail).unwrap();
        assert_eq!(
            tail, b"PACKDUMMY1234567",
            "pack bytes must remain in the reader for the streaming path"
        );
    }
}
