//! `fetch` (protocol v2): request the objects reachable from a set of
//! oids. The server replies with optional negotiation sections and a
//! packfile streamed over sideband.
//!
//! ## Request shape
//!
//! ```text
//! command=fetch\n
//! [object-format=sha1\n]
//! 0001                              (delim — end of command/args header)
//! want <oid>\n                       (one or more)
//! have <oid>\n                       (zero or more)
//! done\n                             (optional — skip negotiation)
//! [no-progress\n]
//! [thin-pack\n]
//! [ofs-delta\n]
//! [include-tag\n]
//! 0000                              (flush)
//! ```
//!
//! ## Response shape
//!
//! Sections (each headed by a name pkt-line, separated by delim-pkts):
//! `acknowledgments` (skipped when `done` was sent), `shallow-info`,
//! `wanted-refs`, `packfile-uris`, `packfile`. Only the `packfile` section
//! is mandatory; the rest are optional and may appear in any order. The
//! response ends with a flush after the packfile sideband stream.
//!
//! Packfile sideband: each pkt-line in the section starts with a one-byte
//! channel — `0x01` pack data, `0x02` progress (stderr-like), `0x03` fatal
//! error.
//!
//! ## API split
//!
//! - [`encode_fetch_request`] writes the request body.
//! - [`read_fetch_preamble`] consumes everything *up to* the `packfile`
//!   section header. The caller then runs [`drain_packfile`] to demux the
//!   sideband and tee the pack bytes into an indexer.
//!
//! The split keeps the packfile section streaming (caller controls the
//! sink — could be a hashing file writer that index-packs as bytes arrive)
//! while the small structured sections come back as plain Rust values.

use std::io::{Read, Write};

use alt_git_codec::{HashAlgo, ObjectId};

use crate::pkt::{self, Frame, PktError};

/// Client-side fetch arguments. Maps directly to protocol v2's optional
/// argument lines (see module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchRequest {
    /// `want <oid>` lines. At least one is required by the spec, but the
    /// caller can also use [`encode_fetch_request`] for testing edge
    /// shapes — the encoder doesn't enforce non-empty.
    pub wants: Vec<ObjectId>,
    /// `have <oid>` lines. Empty on initial clone; populated on incremental
    /// fetch with the client's tip oids.
    pub haves: Vec<ObjectId>,
    /// `done\n` — short-circuit negotiation, server should skip
    /// acknowledgments and send the packfile immediately.
    pub done: bool,
    /// `no-progress\n` — suppress band-2 (progress) sideband output.
    pub no_progress: bool,
    /// `thin-pack\n` — server may emit deltas against bases the client
    /// already has but isn't sending in the pack.
    pub thin_pack: bool,
    /// `ofs-delta\n` — server may use OFS_DELTA (pack-relative) entries.
    pub ofs_delta: bool,
    /// `include-tag\n` — also send annotated tags reachable from `wants`.
    pub include_tag: bool,
}

/// Encode a `fetch` request body. Caller wraps this in the HTTP
/// `POST <repo>/git-upload-pack` body and sets `Content-Type:
/// application/x-git-upload-pack-request` + `Git-Protocol: version=2`.
pub fn encode_fetch_request<W: Write>(
    w: &mut W,
    req: &FetchRequest,
    object_format: Option<&str>,
) -> std::io::Result<()> {
    pkt::write_data(w, b"command=fetch\n")?;
    if let Some(fmt) = object_format {
        pkt::write_data(w, format!("object-format={fmt}\n").as_bytes())?;
    }
    pkt::write_delim(w)?;
    // boolean flags first — spec doesn't pin an order inside the args
    // section, but git canonicalises this way and tests are easier when
    // we match
    if req.thin_pack {
        pkt::write_data(w, b"thin-pack\n")?;
    }
    if req.no_progress {
        pkt::write_data(w, b"no-progress\n")?;
    }
    if req.include_tag {
        pkt::write_data(w, b"include-tag\n")?;
    }
    if req.ofs_delta {
        pkt::write_data(w, b"ofs-delta\n")?;
    }
    for oid in &req.wants {
        pkt::write_data(w, format!("want {oid}\n").as_bytes())?;
    }
    for oid in &req.haves {
        pkt::write_data(w, format!("have {oid}\n").as_bytes())?;
    }
    if req.done {
        pkt::write_data(w, b"done\n")?;
    }
    pkt::write_flush(w)
}

/// One entry in the `acknowledgments` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchAck {
    /// `NAK\n` — none of the `have`s were recognised.
    Nak,
    /// `ACK <oid>\n` — server has this object.
    Ack(ObjectId),
    /// `ready\n` — server has enough commonality and is about to send
    /// the packfile (terminator of the acknowledgments section when it
    /// appears alone).
    Ready,
}

/// One entry in the `shallow-info` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShallowInfo {
    Shallow(ObjectId),
    Unshallow(ObjectId),
}

/// One entry in the `wanted-refs` section (sent when the client asked
/// `want-ref <name>` instead of `want <oid>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedRef {
    pub oid: ObjectId,
    pub name: String,
}

/// Everything the server sent before the `packfile` section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchPreamble {
    /// `acknowledgments` section — empty when the client sent `done`
    /// (server skips negotiation entirely).
    pub acknowledgments: Vec<FetchAck>,
    /// `shallow-info` section.
    pub shallow_info: Vec<ShallowInfo>,
    /// `wanted-refs` section.
    pub wanted_refs: Vec<WantedRef>,
    /// `packfile-uris` section payload lines (rare; partial clone).
    pub packfile_uris: Vec<String>,
    /// Set when the response ended without ever reaching `packfile\n`
    /// (server signalled e.g. "ready" alone for further negotiation, no
    /// pack to drain). The caller should *not* invoke [`drain_packfile`].
    pub packfile_missing: bool,
}

/// Reasons request encoding / response parsing fails.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("pkt-line: {0}")]
    Pkt(#[from] PktError),
    /// A response line wasn't valid UTF-8 or had an unexpected shape for
    /// its section.
    #[error("malformed response line in section {section:?}: {line:?}")]
    BadLine { section: String, line: Vec<u8> },
    /// The server sent an unknown section header (forward-compat: we don't
    /// invent how to parse a new section, but we surface it typed so a
    /// caller can decide).
    #[error("unknown response section: {0:?}")]
    UnknownSection(String),
    /// Sideband band 3 — server reported a fatal error mid-pack.
    #[error("server error (sideband band 3): {0}")]
    ServerError(String),
    /// Sideband channel byte wasn't 1, 2, or 3.
    #[error("invalid sideband channel: {0}")]
    BadSideband(u8),
    /// A packfile sideband pkt-line was empty (no channel byte at all).
    #[error("sideband pkt-line missing channel byte")]
    EmptySidebandPkt,
}

/// Parse a fetch response up to (but not including) the packfile sideband
/// stream. After this returns, the next pkt-lines on `r` are the
/// sideband-wrapped pack bytes — feed them to [`drain_packfile`].
///
/// `algo` is what the client requested in `object-format`; OIDs in the
/// response are parsed against it.
pub fn read_fetch_preamble<R: Read>(
    r: &mut R,
    algo: HashAlgo,
) -> Result<FetchPreamble, FetchError> {
    let mut out = FetchPreamble::default();
    let mut scratch = Vec::new();
    let mut section: Option<&'static str> = None;

    loop {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Flush => {
                // a flush before any 'packfile' header means there's no
                // pack to drain — surface that to the caller
                out.packfile_missing = true;
                return Ok(out);
            }
            Frame::ResponseEnd => {
                out.packfile_missing = true;
                return Ok(out);
            }
            Frame::Delim => {
                // section terminator; next data line is the next section
                // header
                section = None;
                continue;
            }
            Frame::Data(line) => {
                let line_trim = trim_newline(line);
                if section.is_none() {
                    // first line of a section is its name
                    match line_trim {
                        b"acknowledgments" => section = Some("acknowledgments"),
                        b"shallow-info" => section = Some("shallow-info"),
                        b"wanted-refs" => section = Some("wanted-refs"),
                        b"packfile-uris" => section = Some("packfile-uris"),
                        b"packfile" => return Ok(out),
                        other => {
                            return Err(FetchError::UnknownSection(
                                String::from_utf8_lossy(other).into_owned(),
                            ));
                        }
                    }
                    continue;
                }

                match section {
                    Some("acknowledgments") => {
                        out.acknowledgments.push(parse_ack(line_trim, algo)?);
                    }
                    Some("shallow-info") => {
                        out.shallow_info.push(parse_shallow(line_trim, algo)?);
                    }
                    Some("wanted-refs") => {
                        out.wanted_refs.push(parse_wanted_ref(line_trim, algo)?);
                    }
                    Some("packfile-uris") => {
                        out.packfile_uris
                            .push(String::from_utf8_lossy(line_trim).into_owned());
                    }
                    _ => unreachable!("section set by header branch"),
                }
            }
        }
    }
}

/// Read the packfile sideband stream from `r`, writing band-1 (pack data)
/// bytes to `pack_out`, band-2 (progress) bytes to the `progress`
/// callback, and surfacing band-3 (error) as a [`FetchError::ServerError`].
///
/// Returns the number of pack bytes written.
///
/// Termination: a flush packet ends the stream. The function is reentrant
/// across multiple calls only if the caller knows there's another
/// independent sideband stream coming (there isn't in vanilla v2 fetch —
/// this is one-shot).
pub fn drain_packfile<R, W, P>(
    r: &mut R,
    pack_out: &mut W,
    mut progress: P,
) -> Result<u64, FetchError>
where
    R: Read,
    W: Write,
    P: FnMut(&[u8]),
{
    let mut written = 0u64;
    let mut scratch = Vec::new();
    loop {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Flush | Frame::ResponseEnd => return Ok(written),
            Frame::Delim => {
                // a delim mid-pack would mean another section follows;
                // vanilla v2 fetch doesn't do this. Bail loudly rather than
                // truncating the pack silently.
                return Err(FetchError::BadLine {
                    section: "packfile".into(),
                    line: b"unexpected delim".to_vec(),
                });
            }
            Frame::Data(payload) => {
                let Some((&band, body)) = payload.split_first() else {
                    return Err(FetchError::EmptySidebandPkt);
                };
                match band {
                    1 => {
                        pack_out
                            .write_all(body)
                            .map_err(|e| FetchError::Pkt(PktError::Io(e)))?;
                        written += body.len() as u64;
                    }
                    2 => progress(body),
                    3 => {
                        return Err(FetchError::ServerError(
                            String::from_utf8_lossy(body).trim().to_string(),
                        ));
                    }
                    other => return Err(FetchError::BadSideband(other)),
                }
            }
        }
    }
}

fn parse_ack(line: &[u8], algo: HashAlgo) -> Result<FetchAck, FetchError> {
    let s = std::str::from_utf8(line).map_err(|_| FetchError::BadLine {
        section: "acknowledgments".into(),
        line: line.to_vec(),
    })?;
    if s == "NAK" {
        return Ok(FetchAck::Nak);
    }
    if s == "ready" {
        return Ok(FetchAck::Ready);
    }
    if let Some(rest) = s.strip_prefix("ACK ") {
        let oid = parse_oid(rest.trim(), algo, line, "acknowledgments")?;
        return Ok(FetchAck::Ack(oid));
    }
    Err(FetchError::BadLine {
        section: "acknowledgments".into(),
        line: line.to_vec(),
    })
}

fn parse_shallow(line: &[u8], algo: HashAlgo) -> Result<ShallowInfo, FetchError> {
    let s = std::str::from_utf8(line).map_err(|_| FetchError::BadLine {
        section: "shallow-info".into(),
        line: line.to_vec(),
    })?;
    if let Some(rest) = s.strip_prefix("shallow ") {
        return Ok(ShallowInfo::Shallow(parse_oid(
            rest.trim(),
            algo,
            line,
            "shallow-info",
        )?));
    }
    if let Some(rest) = s.strip_prefix("unshallow ") {
        return Ok(ShallowInfo::Unshallow(parse_oid(
            rest.trim(),
            algo,
            line,
            "shallow-info",
        )?));
    }
    Err(FetchError::BadLine {
        section: "shallow-info".into(),
        line: line.to_vec(),
    })
}

fn parse_wanted_ref(line: &[u8], algo: HashAlgo) -> Result<WantedRef, FetchError> {
    let s = std::str::from_utf8(line).map_err(|_| FetchError::BadLine {
        section: "wanted-refs".into(),
        line: line.to_vec(),
    })?;
    let (oid_s, name) = s.split_once(' ').ok_or_else(|| FetchError::BadLine {
        section: "wanted-refs".into(),
        line: line.to_vec(),
    })?;
    Ok(WantedRef {
        oid: parse_oid(oid_s.trim(), algo, line, "wanted-refs")?,
        name: name.trim().to_owned(),
    })
}

fn parse_oid(s: &str, algo: HashAlgo, line: &[u8], section: &str) -> Result<ObjectId, FetchError> {
    s.parse::<ObjectId>()
        .ok()
        .filter(|o| o.algo() == algo)
        .ok_or_else(|| FetchError::BadLine {
            section: section.into(),
            line: line.to_vec(),
        })
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
    use crate::pkt::{write_data, write_delim, write_flush};
    use std::io::Cursor;

    fn sha1(hex: &str) -> ObjectId {
        hex.parse().expect("test oid is valid sha1")
    }

    /// Minimal request with one `want` and `done` round-trips through the
    /// pkt-line decoder in spec order.
    #[test]
    fn request_encodes_in_spec_order() {
        let oid = sha1("0123456789abcdef0123456789abcdef01234567");
        let req = FetchRequest {
            wants: vec![oid],
            done: true,
            ofs_delta: true,
            no_progress: true,
            ..Default::default()
        };
        let mut buf = Vec::new();
        encode_fetch_request(&mut buf, &req, Some("sha1")).unwrap();

        let mut r = Cursor::new(&buf);
        let mut scratch = Vec::new();
        let take = |r: &mut Cursor<&Vec<u8>>, scratch: &mut Vec<u8>| {
            let mut local = Vec::new();
            std::mem::swap(scratch, &mut local);
            let f = pkt::read_frame(r, &mut local).unwrap();
            let out = match f {
                Frame::Data(d) => (b"data".to_vec(), Some(d.to_vec())),
                Frame::Delim => (b"delim".to_vec(), None),
                Frame::Flush => (b"flush".to_vec(), None),
                Frame::ResponseEnd => (b"end".to_vec(), None),
            };
            std::mem::swap(scratch, &mut local);
            out
        };

        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(b"command=fetch\n".to_vec()))
        );
        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(b"object-format=sha1\n".to_vec()))
        );
        assert_eq!(take(&mut r, &mut scratch), (b"delim".to_vec(), None));
        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(b"no-progress\n".to_vec()))
        );
        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(b"ofs-delta\n".to_vec()))
        );
        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(format!("want {oid}\n").into_bytes()),)
        );
        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(b"done\n".to_vec()))
        );
        assert_eq!(take(&mut r, &mut scratch), (b"flush".to_vec(), None));
    }

    /// Bare request (no flags, no haves, no done) still emits
    /// command + delim + want lines + flush.
    #[test]
    fn minimal_request_only_wants() {
        let oid = sha1("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let req = FetchRequest {
            wants: vec![oid],
            ..Default::default()
        };
        let mut buf = Vec::new();
        encode_fetch_request(&mut buf, &req, None).unwrap();
        // command=fetch\n (14 bytes data + 4 header = 0012)
        // 0001 (delim)
        // want <40>\n  (46+4 = 0032)
        // 0000
        let expected = format!("0012command=fetch\n00010032want {oid}\n0000");
        assert_eq!(String::from_utf8(buf).unwrap(), expected);
    }

    /// The canonical "done sent → packfile only" response: just
    /// `packfile\n` then sideband pkt-lines + flush. Preamble parses to
    /// empty acks / shallow / wanted, and `drain_packfile` reads the bytes.
    #[test]
    fn preamble_stops_at_packfile_section_header() {
        let mut buf = Vec::new();
        write_data(&mut buf, b"packfile\n").unwrap();
        // a band-1 pkt: 0x01 + b"PACK..."
        let mut band1 = vec![1u8];
        band1.extend_from_slice(b"PACKDATA");
        write_data(&mut buf, &band1).unwrap();
        write_flush(&mut buf).unwrap();

        let mut r = Cursor::new(&buf);
        let pre = read_fetch_preamble(&mut r, HashAlgo::Sha1).unwrap();
        assert!(pre.acknowledgments.is_empty());
        assert!(pre.shallow_info.is_empty());
        assert!(pre.wanted_refs.is_empty());
        assert!(!pre.packfile_missing);

        let mut sink = Vec::new();
        let n = drain_packfile(&mut r, &mut sink, |_| {}).unwrap();
        assert_eq!(n, b"PACKDATA".len() as u64);
        assert_eq!(sink, b"PACKDATA");
    }

    /// `acknowledgments` + `packfile` (typical incremental fetch without
    /// `done`): acks first, delim, then packfile section.
    #[test]
    fn preamble_collects_acks_and_then_packfile() {
        let h1 = sha1("0123456789abcdef0123456789abcdef01234567");
        let mut buf = Vec::new();
        write_data(&mut buf, b"acknowledgments\n").unwrap();
        write_data(&mut buf, format!("ACK {h1}\n").as_bytes()).unwrap();
        write_data(&mut buf, b"ready\n").unwrap();
        write_delim(&mut buf).unwrap();
        write_data(&mut buf, b"packfile\n").unwrap();
        let mut band1 = vec![1u8];
        band1.extend_from_slice(b"PK");
        write_data(&mut buf, &band1).unwrap();
        write_flush(&mut buf).unwrap();

        let mut r = Cursor::new(&buf);
        let pre = read_fetch_preamble(&mut r, HashAlgo::Sha1).unwrap();
        assert_eq!(
            pre.acknowledgments,
            vec![FetchAck::Ack(h1), FetchAck::Ready]
        );

        let mut sink = Vec::new();
        let n = drain_packfile(&mut r, &mut sink, |_| {}).unwrap();
        assert_eq!(n, 2);
    }

    /// A NAK response (server didn't share any haves) parses cleanly.
    #[test]
    fn nak_acknowledgment_parses() {
        let mut buf = Vec::new();
        write_data(&mut buf, b"acknowledgments\n").unwrap();
        write_data(&mut buf, b"NAK\n").unwrap();
        write_delim(&mut buf).unwrap();
        write_data(&mut buf, b"packfile\n").unwrap();
        write_flush(&mut buf).unwrap();

        let mut r = Cursor::new(&buf);
        let pre = read_fetch_preamble(&mut r, HashAlgo::Sha1).unwrap();
        assert_eq!(pre.acknowledgments, vec![FetchAck::Nak]);
    }

    /// `shallow-info` + `wanted-refs` + `packfile` (kitchen-sink) parse
    /// in the same flow.
    #[test]
    fn preamble_parses_all_section_kinds() {
        let h1 = sha1("0123456789abcdef0123456789abcdef01234567");
        let h2 = sha1("89abcdef0123456789abcdef0123456789abcdef");
        let mut buf = Vec::new();
        write_data(&mut buf, b"shallow-info\n").unwrap();
        write_data(&mut buf, format!("shallow {h1}\n").as_bytes()).unwrap();
        write_data(&mut buf, format!("unshallow {h2}\n").as_bytes()).unwrap();
        write_delim(&mut buf).unwrap();
        write_data(&mut buf, b"wanted-refs\n").unwrap();
        write_data(&mut buf, format!("{h2} refs/heads/main\n").as_bytes()).unwrap();
        write_delim(&mut buf).unwrap();
        write_data(&mut buf, b"packfile\n").unwrap();
        write_flush(&mut buf).unwrap();

        let mut r = Cursor::new(&buf);
        let pre = read_fetch_preamble(&mut r, HashAlgo::Sha1).unwrap();
        assert_eq!(
            pre.shallow_info,
            vec![ShallowInfo::Shallow(h1), ShallowInfo::Unshallow(h2)]
        );
        assert_eq!(
            pre.wanted_refs,
            vec![WantedRef {
                oid: h2,
                name: "refs/heads/main".into(),
            }]
        );
    }

    /// Sideband band 2 (progress) is routed to the callback, not the pack
    /// sink, so progress text doesn't corrupt the packfile.
    #[test]
    fn drain_demuxes_progress_to_callback() {
        let mut buf = Vec::new();
        // band-2 line first, then band-1, then flush
        let mut band2 = vec![2u8];
        band2.extend_from_slice(b"Counting objects: 5\n");
        write_data(&mut buf, &band2).unwrap();
        let mut band1 = vec![1u8];
        band1.extend_from_slice(b"PACK");
        write_data(&mut buf, &band1).unwrap();
        write_flush(&mut buf).unwrap();

        let mut r = Cursor::new(&buf);
        let mut sink = Vec::new();
        let mut progress = Vec::new();
        let n = drain_packfile(&mut r, &mut sink, |b| progress.extend_from_slice(b)).unwrap();
        assert_eq!(n, 4);
        assert_eq!(sink, b"PACK");
        assert_eq!(progress, b"Counting objects: 5\n");
    }

    /// Band 3 surfaces as `FetchError::ServerError` so the caller sees the
    /// server's reason instead of a half-written pack.
    #[test]
    fn drain_band_three_is_server_error() {
        let mut buf = Vec::new();
        let mut band1 = vec![1u8];
        band1.extend_from_slice(b"some");
        write_data(&mut buf, &band1).unwrap();
        let mut band3 = vec![3u8];
        band3.extend_from_slice(b"upload-pack: not our ref\n");
        write_data(&mut buf, &band3).unwrap();

        let mut r = Cursor::new(&buf);
        let mut sink = Vec::new();
        let err = drain_packfile(&mut r, &mut sink, |_| {}).unwrap_err();
        match err {
            FetchError::ServerError(msg) => {
                assert!(msg.contains("not our ref"), "msg = {msg:?}")
            }
            other => panic!("expected ServerError, got {other:?}"),
        }
        // partial pack bytes still landed in sink — caller is expected to
        // discard the temp pack file on error
        assert_eq!(sink, b"some");
    }

    /// An invalid sideband channel byte is a stream-format error, not a
    /// silently-dropped pkt.
    #[test]
    fn drain_rejects_unknown_sideband_channel() {
        let mut buf = Vec::new();
        write_data(&mut buf, &[7u8, b'?']).unwrap();
        let mut r = Cursor::new(&buf);
        let mut sink = Vec::new();
        let err = drain_packfile(&mut r, &mut sink, |_| {}).unwrap_err();
        assert!(matches!(err, FetchError::BadSideband(7)), "{err:?}");
    }

    /// A flush *before* any `packfile\n` header (e.g. server reported only
    /// negotiation hints) surfaces as `packfile_missing = true` so the
    /// caller doesn't blindly call [`drain_packfile`] on an empty stream.
    #[test]
    fn preamble_marks_missing_packfile_when_response_ends_early() {
        let mut buf = Vec::new();
        write_data(&mut buf, b"acknowledgments\n").unwrap();
        write_data(&mut buf, b"NAK\n").unwrap();
        write_flush(&mut buf).unwrap(); // ends response without packfile

        let mut r = Cursor::new(&buf);
        let pre = read_fetch_preamble(&mut r, HashAlgo::Sha1).unwrap();
        assert!(pre.packfile_missing);
        assert_eq!(pre.acknowledgments, vec![FetchAck::Nak]);
    }

    /// An unknown section header is rejected so the parser doesn't drop a
    /// future spec extension on the floor silently.
    #[test]
    fn unknown_section_is_typed_error() {
        let mut buf = Vec::new();
        write_data(&mut buf, b"future-section\n").unwrap();
        let mut r = Cursor::new(&buf);
        let err = read_fetch_preamble(&mut r, HashAlgo::Sha1).unwrap_err();
        assert!(matches!(err, FetchError::UnknownSection(_)), "{err:?}");
    }
}
