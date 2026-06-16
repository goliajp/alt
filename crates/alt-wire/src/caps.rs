//! Protocol v2 capability advertisement.
//!
//! On `GET <repo>/info/refs?service=git-upload-pack` with the
//! `Git-Protocol: version=2` header, the server returns:
//!
//! ```text
//! # service=git-upload-pack\n        (one pkt-line)
//! 0000                              (flush — end of header)
//! version 2\n                        (one pkt-line)
//! agent=git/2.45.0\n                 (one pkt-line)
//! ls-refs=unborn\n                   (one pkt-line)
//! fetch=shallow wait-for-done\n      (one pkt-line)
//! object-format=sha1\n               (one pkt-line)
//! 0000                              (flush — end of capabilities)
//! ```
//!
//! Each capability line is `<name>` for boolean caps or `<name>=<value>` for
//! valued caps (the value may itself contain space-separated sub-features
//! like `fetch=shallow wait-for-done`, with no further structure imposed by
//! the framing). The header pkt-line `# service=…\n` is only present on
//! the smart-http stateless transport; bare TCP / SSH skip it.

use std::collections::BTreeMap;

use crate::pkt::{self, Frame, PktError};

/// What the server advertised.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilityAd {
    /// `version 2` (we only support v2; v0/v1 are an explicit error).
    pub version: u32,
    /// `agent=…`, e.g. `git/2.45.0`. `None` if the server didn't send one
    /// (the agent line is conventional but not required by the spec).
    pub agent: Option<String>,
    /// `object-format=sha1` or `=sha256`. `None` if not advertised — treat
    /// as `sha1` per current git default.
    pub object_format: Option<String>,
    /// All capability lines as `name → value` where `value` is `None` for a
    /// bare `name` line. Stored in a [`BTreeMap`] so the order is
    /// deterministic for snapshot tests.
    pub caps: BTreeMap<String, Option<String>>,
}

impl CapabilityAd {
    /// `true` iff the server advertised this capability (with or without a
    /// value). Use for boolean caps like `unborn`.
    pub fn supports(&self, name: &str) -> bool {
        self.caps.contains_key(name)
    }

    /// The space-separated sub-features of a valued capability, e.g.
    /// `fetch=shallow wait-for-done` → `["shallow", "wait-for-done"]`.
    /// Returns `None` if the cap was not advertised; an empty `Vec` if it
    /// was bare or had no sub-features.
    pub fn sub_features(&self, name: &str) -> Option<Vec<&str>> {
        self.caps.get(name).map(|v| {
            v.as_deref()
                .map(|s| s.split_whitespace().collect())
                .unwrap_or_default()
        })
    }
}

/// Reasons capability parsing fails.
#[derive(Debug, thiserror::Error)]
pub enum CapsError {
    #[error("pkt-line: {0}")]
    Pkt(#[from] PktError),
    /// Header pkt-line was `# service=<name>\n` but `<name>` is wrong.
    #[error("unexpected service header: {0}")]
    BadService(String),
    /// The first non-header pkt-line wasn't `version N\n`.
    #[error("missing version line")]
    MissingVersion,
    /// `version` was something other than `2`. We don't implement v0/v1.
    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u32),
    /// A capability line wasn't UTF-8 or had an unexpected shape.
    #[error("malformed capability line: {0:?}")]
    BadCapability(Vec<u8>),
}

/// Parse a smart-http v2 capability advertisement from a byte stream
/// (typically the body of `GET <repo>/info/refs?service=git-upload-pack`).
/// `expected_service` is `git-upload-pack` for fetch, `git-receive-pack`
/// for push.
pub fn parse_capability_advertisement<R: std::io::Read>(
    r: &mut R,
    expected_service: &str,
) -> Result<CapabilityAd, CapsError> {
    let mut scratch = Vec::new();

    // First frame is *either* the `# service=…\n` header (smart-http) *or*
    // already the `version 2\n` capability line (bare TCP / SSH). We
    // tolerate both shapes — the body is the same after the optional
    // header + its flush.
    let first = pkt::read_frame(r, &mut scratch)?;
    let mut ad = CapabilityAd::default();
    let mut after_header = false;

    match first {
        Frame::Data(d) => {
            if let Some(svc) = d.strip_prefix(b"# service=") {
                let svc = trim_newline(svc);
                let svc = std::str::from_utf8(svc)
                    .map_err(|_| CapsError::BadService(format!("{svc:?}")))?;
                if svc != expected_service {
                    return Err(CapsError::BadService(svc.to_string()));
                }
                // header is followed by a flush before the capability section
                let flush = pkt::read_frame(r, &mut scratch)?;
                if !matches!(flush, Frame::Flush) {
                    return Err(CapsError::BadCapability(
                        b"expected flush after # service=".to_vec(),
                    ));
                }
                after_header = true;
            } else {
                // already a capability line; replay it
                parse_one_cap_line(d, &mut ad)?;
            }
        }
        Frame::Flush => return Err(CapsError::MissingVersion),
        Frame::Delim | Frame::ResponseEnd => {
            return Err(CapsError::BadCapability(b"unexpected delim/end".to_vec()));
        }
    }

    // (If we just consumed the `# service=` header + flush, the next frame
    // is the first capability line.)
    if after_header {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Data(d) => parse_one_cap_line(d, &mut ad)?,
            Frame::Flush => return Err(CapsError::MissingVersion),
            _ => return Err(CapsError::BadCapability(b"unexpected non-data".to_vec())),
        }
    }

    // Now read frames until flush; the `version` line is conventionally
    // first but the spec doesn't pin order — we accept it anywhere in the
    // section as long as exactly one appears.
    loop {
        let mut scratch2 = Vec::new();
        let f = pkt::read_frame(r, &mut scratch2)?;
        match f {
            Frame::Data(d) => parse_one_cap_line(d, &mut ad)?,
            Frame::Flush => break,
            _ => return Err(CapsError::BadCapability(b"unexpected non-data".to_vec())),
        }
    }

    if ad.version == 0 {
        return Err(CapsError::MissingVersion);
    }
    if ad.version != 2 {
        return Err(CapsError::UnsupportedVersion(ad.version));
    }
    Ok(ad)
}

fn parse_one_cap_line(line: &[u8], ad: &mut CapabilityAd) -> Result<(), CapsError> {
    let line = trim_newline(line);
    let s = std::str::from_utf8(line).map_err(|_| CapsError::BadCapability(line.to_vec()))?;
    if let Some(v) = s.strip_prefix("version ") {
        let n: u32 = v
            .trim()
            .parse()
            .map_err(|_| CapsError::BadCapability(line.to_vec()))?;
        ad.version = n;
        return Ok(());
    }
    if let Some(v) = s.strip_prefix("agent=") {
        ad.agent = Some(v.trim().to_owned());
        ad.caps.insert("agent".into(), Some(v.trim().to_owned()));
        return Ok(());
    }
    if let Some(v) = s.strip_prefix("object-format=") {
        ad.object_format = Some(v.trim().to_owned());
        ad.caps
            .insert("object-format".into(), Some(v.trim().to_owned()));
        return Ok(());
    }
    if let Some(eq) = s.find('=') {
        let (name, val) = (&s[..eq], &s[eq + 1..]);
        ad.caps.insert(name.to_owned(), Some(val.trim().to_owned()));
    } else {
        ad.caps.insert(s.to_owned(), None);
    }
    Ok(())
}

fn trim_newline(b: &[u8]) -> &[u8] {
    let mut end = b.len();
    while end > 0 && (b[end - 1] == b'\n' || b[end - 1] == b'\r') {
        end -= 1;
    }
    &b[..end]
}

/// Encode a smart-http v2 capability advertisement for `service` (e.g.
/// `git-upload-pack`). M9/W10a — the server's first response on
/// `GET <repo>/info/refs?service=…`. The body is exactly the bytes a
/// client's [`parse_capability_advertisement`] reads.
///
/// Layout (per gitprotocol-v2.txt §1.1):
///
///   pkt: "# service=<svc>\n"
///   flush
///   pkt: "version 2\n"
///   pkt: "agent=<agent>\n"
///   pkt: "object-format=<fmt>\n"   (when provided)
///   pkt: each command name with optional sub-features
///   flush
pub fn encode_capability_advertisement<W: std::io::Write>(
    w: &mut W,
    service: &str,
    agent: &str,
    object_format: Option<&str>,
    commands: &[(&str, Option<&str>)],
) -> std::io::Result<()> {
    pkt::write_data(w, format!("# service={service}\n").as_bytes())?;
    pkt::write_flush(w)?;
    pkt::write_data(w, b"version 2\n")?;
    pkt::write_data(w, format!("agent={agent}\n").as_bytes())?;
    if let Some(fmt) = object_format {
        pkt::write_data(w, format!("object-format={fmt}\n").as_bytes())?;
    }
    for (cmd, features) in commands {
        match features {
            Some(f) if !f.is_empty() => pkt::write_data(w, format!("{cmd}={f}\n").as_bytes())?,
            _ => pkt::write_data(w, format!("{cmd}\n").as_bytes())?,
        }
    }
    pkt::write_flush(w)
}

#[cfg(test)]
mod server_tests {
    use super::*;

    #[test]
    fn server_advert_round_trips_through_parse_capability_advertisement() {
        let mut bytes = Vec::new();
        encode_capability_advertisement(
            &mut bytes,
            "git-upload-pack",
            "alt-server/0.0.0",
            Some("sha1"),
            &[
                ("ls-refs", Some("unborn")),
                ("fetch", Some("shallow wait-for-done")),
            ],
        )
        .unwrap();
        let ad =
            parse_capability_advertisement(&mut std::io::Cursor::new(&bytes), "git-upload-pack")
                .unwrap();
        assert_eq!(ad.version, 2);
        assert_eq!(ad.agent.as_deref(), Some("alt-server/0.0.0"));
        assert_eq!(ad.object_format.as_deref(), Some("sha1"));
        assert!(ad.supports("ls-refs"));
        assert_eq!(ad.sub_features("ls-refs").unwrap(), vec!["unborn"]);
        assert_eq!(
            ad.sub_features("fetch").unwrap(),
            vec!["shallow", "wait-for-done"]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkt::{write_data, write_flush};
    use std::io::Cursor;

    fn build(lines: &[&[u8]], smart_http: bool, service: &str) -> Vec<u8> {
        let mut out = Vec::new();
        if smart_http {
            write_data(&mut out, format!("# service={service}\n").as_bytes()).unwrap();
            write_flush(&mut out).unwrap();
        }
        for l in lines {
            write_data(&mut out, l).unwrap();
        }
        write_flush(&mut out).unwrap();
        out
    }

    /// The canonical smart-http v2 advertisement: header, flush, version,
    /// agent, valued caps, object-format, flush. Round-trips cleanly.
    #[test]
    fn parses_canonical_smart_http_advertisement() {
        let bytes = build(
            &[
                b"version 2\n",
                b"agent=git/2.45.0\n",
                b"ls-refs=unborn\n",
                b"fetch=shallow wait-for-done\n",
                b"object-format=sha1\n",
            ],
            true,
            "git-upload-pack",
        );
        let ad =
            parse_capability_advertisement(&mut Cursor::new(&bytes), "git-upload-pack").unwrap();
        assert_eq!(ad.version, 2);
        assert_eq!(ad.agent.as_deref(), Some("git/2.45.0"));
        assert_eq!(ad.object_format.as_deref(), Some("sha1"));
        assert!(ad.supports("ls-refs"));
        assert!(ad.supports("fetch"));
        assert!(!ad.supports("nope"));
        assert_eq!(
            ad.sub_features("fetch"),
            Some(vec!["shallow", "wait-for-done"])
        );
        assert_eq!(ad.sub_features("ls-refs"), Some(vec!["unborn"]));
        assert_eq!(ad.sub_features("agent"), Some(vec!["git/2.45.0"]));
    }

    /// Bare TCP / SSH transport sends no `# service=` header — the body
    /// starts with `version 2\n`. We accept both shapes.
    #[test]
    fn parses_advertisement_without_service_header() {
        let bytes = build(
            &[b"version 2\n", b"agent=alt/0.0\n"],
            false,
            "git-upload-pack",
        );
        let ad =
            parse_capability_advertisement(&mut Cursor::new(&bytes), "git-upload-pack").unwrap();
        assert_eq!(ad.version, 2);
        assert_eq!(ad.agent.as_deref(), Some("alt/0.0"));
    }

    /// A wrong `# service=` header is rejected (not silently ignored), so
    /// `alt push` against an `upload-pack` URL fires a clear error.
    #[test]
    fn rejects_wrong_service_header() {
        let bytes = build(
            &[b"version 2\n"],
            /* smart_http */ true,
            "git-upload-pack",
        );
        let err = parse_capability_advertisement(&mut Cursor::new(&bytes), "git-receive-pack")
            .unwrap_err();
        assert!(matches!(err, CapsError::BadService(_)), "{err:?}");
    }

    /// `version 1` (or any non-2) is an explicit error so we never silently
    /// fall back into a protocol we don't implement.
    #[test]
    fn rejects_unsupported_protocol_version() {
        let bytes = build(&[b"version 1\n"], false, "git-upload-pack");
        let err = parse_capability_advertisement(&mut Cursor::new(&bytes), "git-upload-pack")
            .unwrap_err();
        assert!(matches!(err, CapsError::UnsupportedVersion(1)), "{err:?}");
    }

    /// Bare capabilities (no `=`) decode as `name → None`. `supports` is
    /// the right check for these.
    #[test]
    fn bare_capability_lines_decode_as_none_value() {
        let bytes = build(
            &[b"version 2\n", b"server-option\n"],
            false,
            "git-upload-pack",
        );
        let ad =
            parse_capability_advertisement(&mut Cursor::new(&bytes), "git-upload-pack").unwrap();
        assert!(ad.supports("server-option"));
        assert_eq!(ad.sub_features("server-option"), Some(Vec::<&str>::new()));
    }

    /// A stream that ends mid-section surfaces as a pkt-line error, not a
    /// silent half-parsed [`CapabilityAd`].
    #[test]
    fn truncated_stream_is_an_error_not_partial_caps() {
        // header + flush + `version 2\n`, but no closing flush
        let mut bytes = Vec::new();
        write_data(&mut bytes, b"# service=git-upload-pack\n").unwrap();
        write_flush(&mut bytes).unwrap();
        write_data(&mut bytes, b"version 2\n").unwrap();
        // no trailing flush — stream ends here
        let err = parse_capability_advertisement(&mut Cursor::new(&bytes), "git-upload-pack")
            .unwrap_err();
        assert!(matches!(err, CapsError::Pkt(_)), "{err:?}");
    }
}
