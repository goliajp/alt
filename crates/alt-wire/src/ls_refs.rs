//! `ls-refs` (protocol v2): one request, one ref list back. The smallest
//! useful round-trip — `alt remote ls` and the first step of every fetch
//! both speak this.
//!
//! ## Request shape
//!
//! ```text
//! command=ls-refs\n
//! object-format=sha1\n               (optional; servers default to sha1)
//! 0001                              (delim — end of command/args header)
//! peel\n                             (zero or more arg lines)
//! symrefs\n
//! ref-prefix refs/heads/\n
//! 0000                              (flush)
//! ```
//!
//! ## Response shape
//!
//! ```text
//! <oid> <refname>\n                  (one pkt-line per ref)
//! <oid> <refname> symref-target:HEAD peeled:<oid>\n
//! 0000                              (flush)
//! ```
//!
//! Symref / peeled annotations are space-separated `key:value` pairs after
//! the refname. We parse them generically so a peer that grows new ones
//! doesn't break old alt clients.

use std::collections::BTreeMap;

use alt_git_codec::{HashAlgo, ObjectId};

use crate::pkt::{self, Frame, PktError};

/// Client-side request shape. The fields map directly to protocol v2's
/// optional arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LsRefsRequest {
    /// `symrefs\n` — request symbolic-ref resolution annotations
    /// (`symref-target:<name>`).
    pub symrefs: bool,
    /// `peel\n` — request peeled tag oids (`peeled:<oid>`).
    pub peel: bool,
    /// Each `ref-prefix <p>\n` line: only refs matching one of these
    /// prefixes are returned. Empty list = all refs.
    pub ref_prefixes: Vec<String>,
}

/// One server-side ref-list entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefRecord {
    /// The ref's resolved target oid.
    pub oid: ObjectId,
    /// The full ref name (e.g. `refs/heads/main`).
    pub name: String,
    /// If `symrefs` was requested and this ref is a symbolic ref, the
    /// target ref name (e.g. `refs/heads/main` for `HEAD`).
    pub symref_target: Option<String>,
    /// If `peel` was requested and this is an annotated tag, the peeled
    /// commit oid.
    pub peeled: Option<ObjectId>,
    /// Any other `key:value` annotations the server sent (forward-compat:
    /// a new git release can grow ref attributes and old alt clients still
    /// parse the rest).
    pub other: BTreeMap<String, String>,
}

/// Errors from request encoding / response parsing.
#[derive(Debug, thiserror::Error)]
pub enum LsRefsError {
    #[error("pkt-line: {0}")]
    Pkt(#[from] PktError),
    /// A line in the response wasn't `<oid> <name>[…]`.
    #[error("malformed ref line: {0:?}")]
    BadRefLine(Vec<u8>),
    /// The advertised `object-format` wasn't one we know how to parse.
    #[error("unsupported object format: {0}")]
    BadObjectFormat(String),
}

/// Encode an `ls-refs` request body. The caller wraps this in the HTTP
/// `POST <repo>/git-upload-pack` body and sets `Content-Type:
/// application/x-git-upload-pack-request`.
pub fn encode_ls_refs_request<W: std::io::Write>(
    w: &mut W,
    req: &LsRefsRequest,
    object_format: Option<&str>,
) -> std::io::Result<()> {
    pkt::write_data(w, b"command=ls-refs\n")?;
    if let Some(fmt) = object_format {
        pkt::write_data(w, format!("object-format={fmt}\n").as_bytes())?;
    }
    pkt::write_delim(w)?;
    if req.peel {
        pkt::write_data(w, b"peel\n")?;
    }
    if req.symrefs {
        pkt::write_data(w, b"symrefs\n")?;
    }
    for prefix in &req.ref_prefixes {
        pkt::write_data(w, format!("ref-prefix {prefix}\n").as_bytes())?;
    }
    pkt::write_flush(w)
}

/// Parse an `ls-refs` response. The server sends one pkt-line per ref
/// followed by a flush; this function drains the stream up to that flush.
/// `algo` is what the request asked for (`object-format=…`), defaulting
/// to SHA-1.
pub fn parse_ls_refs_response<R: std::io::Read>(
    r: &mut R,
    algo: HashAlgo,
) -> Result<Vec<RefRecord>, LsRefsError> {
    let mut refs = Vec::new();
    let mut scratch = Vec::new();
    loop {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Flush => break,
            Frame::Data(line) => refs.push(parse_ref_line(line, algo)?),
            // The server can in principle send delim/response-end inside a
            // command response (it doesn't for ls-refs, but we shouldn't
            // crash if it does — just stop, the caller can re-sync).
            Frame::Delim | Frame::ResponseEnd => break,
        }
    }
    Ok(refs)
}

fn parse_ref_line(line: &[u8], algo: HashAlgo) -> Result<RefRecord, LsRefsError> {
    let line = trim_newline(line);
    let s = std::str::from_utf8(line).map_err(|_| LsRefsError::BadRefLine(line.to_vec()))?;
    let mut tokens = s.split(' ');
    let oid_s = tokens
        .next()
        .ok_or_else(|| LsRefsError::BadRefLine(line.to_vec()))?;
    let name = tokens
        .next()
        .ok_or_else(|| LsRefsError::BadRefLine(line.to_vec()))?;
    let oid = parse_oid(oid_s, algo, line)?;
    let mut record = RefRecord {
        oid,
        name: name.to_owned(),
        symref_target: None,
        peeled: None,
        other: BTreeMap::new(),
    };
    for tok in tokens {
        let Some(colon) = tok.find(':') else {
            // bare attribute: stash under `other` with empty value
            record.other.insert(tok.to_owned(), String::new());
            continue;
        };
        let (k, v) = (&tok[..colon], &tok[colon + 1..]);
        match k {
            "symref-target" => record.symref_target = Some(v.to_owned()),
            "peeled" => record.peeled = Some(parse_oid(v, algo, line)?),
            _ => {
                record.other.insert(k.to_owned(), v.to_owned());
            }
        }
    }
    Ok(record)
}

fn parse_oid(s: &str, algo: HashAlgo, line: &[u8]) -> Result<ObjectId, LsRefsError> {
    s.parse::<ObjectId>()
        .ok()
        .filter(|o| o.algo() == algo)
        .ok_or_else(|| LsRefsError::BadRefLine(line.to_vec()))
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
        hex.parse().expect("test oid is valid sha1")
    }

    /// A request with all options round-trips through the pkt-line encoder
    /// in the spec order (command, object-format, delim, args, flush).
    #[test]
    fn request_encodes_in_spec_order() {
        let req = LsRefsRequest {
            symrefs: true,
            peel: true,
            ref_prefixes: vec!["refs/heads/".into(), "refs/tags/".into()],
        };
        let mut buf = Vec::new();
        encode_ls_refs_request(&mut buf, &req, Some("sha1")).unwrap();

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
            (b"data".to_vec(), Some(b"command=ls-refs\n".to_vec()))
        );
        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(b"object-format=sha1\n".to_vec()))
        );
        assert_eq!(take(&mut r, &mut scratch), (b"delim".to_vec(), None));
        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(b"peel\n".to_vec()))
        );
        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(b"symrefs\n".to_vec()))
        );
        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(b"ref-prefix refs/heads/\n".to_vec()))
        );
        assert_eq!(
            take(&mut r, &mut scratch),
            (b"data".to_vec(), Some(b"ref-prefix refs/tags/\n".to_vec()))
        );
        assert_eq!(take(&mut r, &mut scratch), (b"flush".to_vec(), None));
    }

    /// The minimal request (no symrefs / peel / prefixes) still emits
    /// command + delim + flush — the args section is empty but present.
    #[test]
    fn minimal_request_still_has_args_section() {
        let req = LsRefsRequest::default();
        let mut buf = Vec::new();
        encode_ls_refs_request(&mut buf, &req, None).unwrap();
        // command=ls-refs\n + 0001 (delim) + 0000 (flush)
        assert_eq!(buf, b"0014command=ls-refs\n00010000");
    }

    /// A canonical response: one ref with no extras, one HEAD with symref +
    /// peeled annotations, and forward-compat unknown attribute.
    #[test]
    fn response_parses_records_with_annotations() {
        let h1 = sha1("0123456789abcdef0123456789abcdef01234567");
        let h2 = sha1("89abcdef0123456789abcdef0123456789abcdef");
        let h3 = sha1("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let mut buf = Vec::new();
        write_data(&mut buf, format!("{h1} refs/heads/main\n").as_bytes()).unwrap();
        write_data(
            &mut buf,
            format!("{h2} HEAD symref-target:refs/heads/main\n").as_bytes(),
        )
        .unwrap();
        write_data(
            &mut buf,
            format!("{h3} refs/tags/v1 peeled:{h1} future-attr:42\n").as_bytes(),
        )
        .unwrap();
        write_flush(&mut buf).unwrap();

        let refs = parse_ls_refs_response(&mut Cursor::new(&buf), HashAlgo::Sha1).unwrap();
        assert_eq!(refs.len(), 3);

        assert_eq!(refs[0].name, "refs/heads/main");
        assert_eq!(refs[0].oid, h1);
        assert_eq!(refs[0].symref_target, None);
        assert_eq!(refs[0].peeled, None);

        assert_eq!(refs[1].name, "HEAD");
        assert_eq!(refs[1].symref_target.as_deref(), Some("refs/heads/main"));

        assert_eq!(refs[2].name, "refs/tags/v1");
        assert_eq!(refs[2].peeled, Some(h1));
        // forward-compat: a future server-added attribute lands in `other`,
        // not a hard error
        assert_eq!(
            refs[2].other.get("future-attr").map(String::as_str),
            Some("42")
        );
    }

    /// An OID that doesn't match the requested algo (wrong length / hex)
    /// is a hard error so the caller doesn't silently treat junk as a ref.
    #[test]
    fn wrong_oid_algo_is_rejected() {
        // sha256 length, but requested sha1
        let mut buf = Vec::new();
        write_data(
            &mut buf,
            b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef refs/heads/x\n",
        )
        .unwrap();
        write_flush(&mut buf).unwrap();
        let err = parse_ls_refs_response(&mut Cursor::new(&buf), HashAlgo::Sha1).unwrap_err();
        assert!(matches!(err, LsRefsError::BadRefLine(_)), "{err:?}");
    }

    /// An empty response (just a flush) is fine — a freshly-init'd repo
    /// returns zero refs.
    #[test]
    fn empty_response_returns_empty_vec() {
        let mut buf = Vec::new();
        write_flush(&mut buf).unwrap();
        let refs = parse_ls_refs_response(&mut Cursor::new(&buf), HashAlgo::Sha1).unwrap();
        assert!(refs.is_empty());
    }
}
