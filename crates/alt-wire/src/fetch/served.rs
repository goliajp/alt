//! The server's side of `fetch`: the request read, the pack framed.

use std::io::{Read, Write};

use alt_git_codec::{HashAlgo, ObjectId};

use super::{FetchError, FetchRequest, trim_newline};
use crate::pkt::{self, Frame};

/// The most `want` lines of a request. Git sends one for each ref it
/// asks for, and a partial clone one for each object it left out that a
/// checkout needs, all in one request: hundreds of thousands in a large
/// tree.
pub const MAX_WANTS: usize = 1 << 20;
/// The most `have` lines of a request. Git sends them in rounds that
/// grow to some sixteen thousand, and gives up a negotiation that finds
/// nothing in common long before this.
pub const MAX_HAVES: usize = 1 << 18;

/// Server-side parse of a `fetch` request body. Mirror image
/// of [`encode_fetch_request`]: reads `command=fetch` + optional
/// `object-format=…`, the delim, the boolean args + want/have/done
/// lines, then the trailing flush.
///
/// Returns the parsed request together with the `object-format`
/// discriminator the client asked for (defaulting to `None` — the
/// server should treat as `sha1`).
///
/// Each want and each have is in the request once, however often its
/// line was sent, and a request with more of either than [`MAX_WANTS`]
/// and [`MAX_HAVES`] is refused.
pub fn parse_fetch_request<R: Read>(
    r: &mut R,
) -> Result<(FetchRequest, Option<String>), FetchError> {
    let mut req = FetchRequest::default();
    let mut object_format = None;
    let algo = HashAlgo::Sha1; // for parsing want/have oid hex; sha256 lands when we wire it
    let mut scratch = Vec::new();
    // Pre-delim header: command + optional object-format.
    loop {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Delim => break,
            Frame::Flush => return Ok((req, object_format)),
            Frame::ResponseEnd => return Ok((req, object_format)),
            Frame::Data(line) => {
                let line = trim_newline(line);
                let s = std::str::from_utf8(line).map_err(|_| FetchError::BadLine {
                    section: "header".into(),
                    line: line.to_vec(),
                })?;
                if s == "command=fetch" {
                    continue;
                }
                if let Some(v) = s.strip_prefix("object-format=") {
                    object_format = Some(v.trim().to_owned());
                }
                // unknown header lines: ignore — forward compatibility
            }
        }
    }
    // Post-delim args: flags + want/have/done.
    loop {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Flush => break,
            Frame::Delim | Frame::ResponseEnd => break,
            Frame::Data(line) => {
                let line = trim_newline(line);
                let s = std::str::from_utf8(line).map_err(|_| FetchError::BadLine {
                    section: "args".into(),
                    line: line.to_vec(),
                })?;
                match s {
                    "done" => req.done = true,
                    "no-progress" => req.no_progress = true,
                    "thin-pack" => req.thin_pack = true,
                    "ofs-delta" => req.ofs_delta = true,
                    "include-tag" => req.include_tag = true,
                    "wait-for-done" => req.wait_for_done = true,
                    _ if req.depth.line(s, line)? => {}
                    _ => {
                        if let Some(hex) = s.strip_prefix("want ") {
                            let oid = parse_oid_str(hex.trim(), algo, line)?;
                            push_within(&mut req.wants, oid, MAX_WANTS, "want")?;
                        } else if let Some(hex) = s.strip_prefix("have ") {
                            let oid = parse_oid_str(hex.trim(), algo, line)?;
                            push_within(&mut req.haves, oid, MAX_HAVES, "have")?;
                        } else if let Some(spec) = s.strip_prefix("filter ") {
                            req.filter = Some(spec.trim().to_owned());
                        }
                        // unknown arg lines: ignore (forward compat)
                    }
                }
            }
        }
    }
    for ids in [&mut req.wants, &mut req.haves] {
        ids.sort_unstable();
        ids.dedup();
    }
    Ok((req, object_format))
}

fn push_within(
    ids: &mut Vec<ObjectId>,
    oid: ObjectId,
    most: usize,
    what: &'static str,
) -> Result<(), FetchError> {
    if ids.len() >= most {
        return Err(FetchError::TooMany(what));
    }
    ids.push(oid);
    Ok(())
}

fn parse_oid_str(s: &str, algo: HashAlgo, line: &[u8]) -> Result<ObjectId, FetchError> {
    s.parse::<ObjectId>()
        .ok()
        .filter(|o| o.algo() == algo)
        .ok_or_else(|| FetchError::BadLine {
            section: "want/have".into(),
            line: line.to_vec(),
        })
}

/// Server-side encode of a `fetch` response that only carries a
/// packfile section (client sends `done` so the
/// server skips the acknowledgments section and goes straight to the
/// pack). Wraps `pack_bytes` in v2's sideband framing:
///
///   pkt: "packfile\n"
///   loop over `pack_bytes` in ≤65515-byte chunks:
///     pkt: "\x01" + chunk        # band 1 = pack data
///   flush
pub fn encode_fetch_response_packfile<W: Write>(
    w: &mut W,
    pack_bytes: &[u8],
) -> std::io::Result<()> {
    pkt::write_data(w, b"packfile\n")?;
    // pkt-line payload max = MAX_LINE_LEN; minus the 4-byte length prefix
    // pkt::write_data uses + the 1-byte sideband channel discriminator we
    // prepend, leaves this much room for actual pack bytes per frame.
    const SIDEBAND_CHUNK: usize = crate::pkt::MAX_LINE_LEN - 5;
    for chunk in pack_bytes.chunks(SIDEBAND_CHUNK) {
        let mut framed = Vec::with_capacity(chunk.len() + 1);
        framed.push(0x01); // band 1: pack data
        framed.extend_from_slice(chunk);
        pkt::write_data(w, &framed)?;
    }
    pkt::write_flush(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkt::{write_data, write_delim, write_flush};

    fn request(lines: impl Iterator<Item = String>) -> Vec<u8> {
        let mut body = Vec::new();
        write_data(&mut body, b"command=fetch\n").unwrap();
        write_delim(&mut body).unwrap();
        for line in lines {
            write_data(&mut body, line.as_bytes()).unwrap();
        }
        write_data(&mut body, b"done\n").unwrap();
        write_flush(&mut body).unwrap();
        body
    }

    fn id(n: usize) -> String {
        format!("{n:040x}")
    }

    #[test]
    fn a_want_or_a_have_sent_many_times_is_in_the_request_once() {
        let lines = (0..9000).map(|n| match n % 3 {
            0 => format!("want {}\n", id(1 + n % 2)),
            1 => format!("have {}\n", id(7)),
            _ => format!("want {}\n", id(1)),
        });
        let (parsed, _) = parse_fetch_request(&mut &request(lines)[..]).unwrap();
        let ids = |of: &[ObjectId]| of.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(ids(&parsed.wants), [id(1), id(2)]);
        assert_eq!(ids(&parsed.haves), [id(7)]);
        assert!(parsed.done);
    }

    #[test]
    fn a_request_of_more_lines_of_a_kind_than_a_server_takes_is_refused() {
        for (what, most) in [("want", MAX_WANTS), ("have", MAX_HAVES)] {
            let lines = |count: usize| (0..count).map(move |_| format!("{what} {}\n", id(1)));
            let whole = parse_fetch_request(&mut &request(lines(most))[..])
                .unwrap()
                .0;
            assert_eq!(whole.wants.len() + whole.haves.len(), 1);
            let over = parse_fetch_request(&mut &request(lines(most + 1))[..]);
            assert!(matches!(over, Err(FetchError::TooMany(kind)) if kind == what));
        }
    }
}
