//! What the server of a push writes and reads: the refs it advertises, the
//! commands it is sent, and the report it answers with.

use std::io::{Read, Write};

use alt_git_codec::{HashAlgo, ObjectId};

use super::{CommandStatus, PushError, RefUpdate, trim_newline, zero_oid};
use crate::pkt::{self, Frame};

/// Server-side encode of a v1 ref advertisement. Mirror of
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
/// Split from [`parse_push_request`] into this head + a separate
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
/// be many gigabytes — the server streams it).
pub fn parse_push_request_head<R: Read>(r: &mut R, algo: HashAlgo) -> Result<PushHead, PushError> {
    parse_push_head(r, algo, usize::MAX, usize::MAX, false)
}

/// Bounded receive-pack command parsing for network ingress. Capabilities are
/// accepted only on the first command, and only a flush terminates the head.
pub fn parse_push_request_head_bounded<R: Read>(
    r: &mut R,
    algo: HashAlgo,
    max_updates: usize,
    max_capability_bytes: usize,
) -> Result<PushHead, PushError> {
    parse_push_head(r, algo, max_updates, max_capability_bytes, true)
}
fn parse_push_head<R: Read>(
    r: &mut R,
    algo: HashAlgo,
    max_updates: usize,
    max_capability_bytes: usize,
    strict: bool,
) -> Result<PushHead, PushError> {
    let mut updates: Vec<RefUpdate> = Vec::new();
    let mut capabilities: Vec<String> = Vec::new();
    let mut scratch = Vec::new();
    let zero = zero_oid(algo);
    loop {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Flush => break,
            Frame::Delim | Frame::ResponseEnd => {
                if strict {
                    return Err(PushError::BadRefLine(b"expected command flush".to_vec()));
                }
                break;
            }
            Frame::Data(line) => {
                if updates.len() >= max_updates {
                    return Err(PushError::BadRefLine(b"command count limit".to_vec()));
                }
                let trimmed = trim_newline(line);
                let (head, caps_part) = match trimmed.iter().position(|&b| b == 0) {
                    Some(i) => (&trimmed[..i], Some(&trimmed[i + 1..])),
                    None => (trimmed, None),
                };
                if let Some(caps) = caps_part
                    && (caps.len() > max_capability_bytes
                        || (strict && (!updates.is_empty() || caps.contains(&0))))
                {
                    return Err(PushError::BadRefLine(
                        b"invalid command capabilities".to_vec(),
                    ));
                }
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

/// Server-side parse of a `git-receive-pack` POST body.
/// Mirror of [`encode_push_request`]: reads the ref-update lines + the
/// (optional, NUL-attached) capability list, then drains everything
/// after the trailing flush as the raw pack stream.
///
/// Prefer [`parse_push_request_head`] + a direct read
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

/// Server-side encode of `report-status`. Plain pkt-lines —
/// no sideband — because the client only switches to sideband if the
/// server advertised `side-band-64k`; this encoder doesn't, so we keep the
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
/// framing. Use this when the client advertised `side-band-64k`
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
