//! The server's side of `ls-refs`: the request read, the refs listed.

use super::{LsRefsError, LsRefsRequest, RefRecord, trim_newline};
use crate::pkt::{self, Frame};

/// Server side of ls-refs: parse the request the client sent.
/// Layout (per gitprotocol-v2.txt):
///
///   pkt: "command=ls-refs\n"
///   pkt: "object-format=<fmt>\n"   (optional)
///   delim
///   pkt: "peel\n"                  (optional, in any order)
///   pkt: "symrefs\n"               (optional)
///   pkt: "ref-prefix <p>\n"        (zero or more)
///   flush
///
/// Returns the request struct plus the object-format string the client
/// asked for (defaulting to None — server should treat as sha1).
pub fn parse_ls_refs_request<R: std::io::Read>(
    r: &mut R,
) -> Result<(LsRefsRequest, Option<String>), LsRefsError> {
    let mut req = LsRefsRequest::default();
    let mut object_format = None;
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
                let s = std::str::from_utf8(line)
                    .map_err(|_| LsRefsError::BadRefLine(line.to_vec()))?;
                if s == "command=ls-refs" {
                    continue;
                }
                if let Some(v) = s.strip_prefix("object-format=") {
                    object_format = Some(v.trim().to_owned());
                }
                // unknown header lines: ignore — forward compatibility
            }
        }
    }
    let mut prefixes = 0usize;
    // Post-delim args: peel / symrefs / ref-prefix.
    loop {
        let f = pkt::read_frame(r, &mut scratch)?;
        match f {
            Frame::Flush => break,
            Frame::Delim | Frame::ResponseEnd => break,
            Frame::Data(line) => {
                let line = trim_newline(line);
                let s = std::str::from_utf8(line)
                    .map_err(|_| LsRefsError::BadRefLine(line.to_vec()))?;
                match s {
                    "peel" => req.peel = true,
                    "symrefs" => req.symrefs = true,
                    _ => {
                        if let Some(p) = s.strip_prefix("ref-prefix ") {
                            prefixes += 1;
                            if prefixes < MAX_PREFIXES {
                                req.ref_prefixes.push(p.to_owned());
                            }
                        }
                        // unknown args: ignore
                    }
                }
            }
        }
    }
    // as Git has it: a request with too many prefixes is given every ref,
    // which holds all it asked for
    if prefixes >= MAX_PREFIXES {
        req.ref_prefixes.clear();
    }
    Ok((req, object_format))
}

/// The number of `ref-prefix` lines from which on a request is answered
/// with every ref: `TOO_MANY_PREFIXES` of Git's `ls-refs.c`, where a
/// request of that many has its prefixes dropped. Git sends a handful for
/// each refspec of a fetch.
pub const MAX_PREFIXES: usize = 65536;

impl LsRefsRequest {
    /// The prefixes in an order and a number in which one look tells
    /// whether a name has one of them: sorted, none with another of them
    /// as its own prefix.
    pub fn prefixes(&self) -> Vec<&str> {
        let mut sorted: Vec<&str> = self.ref_prefixes.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        let mut kept: Vec<&str> = Vec::with_capacity(sorted.len());
        for prefix in sorted {
            if !kept.last().is_some_and(|last| prefix.starts_with(last)) {
                kept.push(prefix);
            }
        }
        kept
    }

    /// Whether `name` has one of `prefixes`, which [`Self::prefixes`]
    /// gave; every name has where there are none.
    pub fn listed(prefixes: &[&str], name: &str) -> bool {
        // any prefix of the name sorts at or before it, and what lies
        // between the two has that prefix as well
        let before = prefixes.partition_point(|prefix| *prefix <= name);
        prefixes.is_empty() || (before > 0 && name.starts_with(prefixes[before - 1]))
    }
}

/// Server side of ls-refs: encode the response — one pkt-line
/// per ref then a flush — for a list of [`RefRecord`]s the server
/// computed from its store. Matches [`parse_ls_refs_response`] byte-for-
/// byte: a server's encoded refs round-trip through the client's parser.
pub fn encode_ls_refs_response<W: std::io::Write>(
    w: &mut W,
    refs: &[RefRecord],
) -> std::io::Result<()> {
    let mut line = Vec::new();
    for r in refs {
        line.clear();
        line.extend_from_slice(r.oid.to_string().as_bytes());
        line.push(b' ');
        line.extend_from_slice(r.name.as_bytes());
        if let Some(t) = &r.symref_target {
            line.extend_from_slice(b" symref-target:");
            line.extend_from_slice(t.as_bytes());
        }
        if let Some(p) = &r.peeled {
            line.extend_from_slice(b" peeled:");
            line.extend_from_slice(p.to_string().as_bytes());
        }
        for (k, v) in &r.other {
            line.push(b' ');
            line.extend_from_slice(k.as_bytes());
            if !v.is_empty() {
                line.push(b':');
                line.extend_from_slice(v.as_bytes());
            }
        }
        line.push(b'\n');
        pkt::write_data(w, &line)?;
    }
    pkt::write_flush(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkt::{write_data, write_delim, write_flush};

    fn request(prefixes: impl Iterator<Item = String>) -> LsRefsRequest {
        let mut body = Vec::new();
        write_data(&mut body, b"command=ls-refs\n").unwrap();
        write_delim(&mut body).unwrap();
        for prefix in prefixes {
            write_data(&mut body, format!("ref-prefix {prefix}\n").as_bytes()).unwrap();
        }
        write_flush(&mut body).unwrap();
        parse_ls_refs_request(&mut &body[..]).unwrap().0
    }

    /// Every way of choosing among these prefixes lists the names that
    /// have one of the chosen, as looking through all of them tells.
    #[test]
    fn a_name_is_listed_where_it_has_one_of_the_prefixes() {
        let prefixes = [
            "",
            "refs/",
            "refs/heads/",
            "refs/heads/m",
            "refs/tags/",
            "HEAD",
            "refs/headz",
        ];
        let names = [
            "HEAD",
            "HEAD2",
            "refs/heads/main",
            "refs/heads/next",
            "refs/head",
            "refs/heads",
            "refs/headz/a",
            "refs/tags/v1",
            "refs0",
            "a",
            "zz",
        ];
        for chosen in 0u32..1 << prefixes.len() {
            let of = |at: &usize| chosen & 1 << at != 0;
            let picked: Vec<&str> = (0..prefixes.len())
                .filter(of)
                .map(|at| prefixes[at])
                .collect();
            let req = request(picked.iter().map(|prefix| (*prefix).to_owned()));
            let sorted = req.prefixes();
            for name in names {
                let has = picked.is_empty() || picked.iter().any(|prefix| name.starts_with(prefix));
                assert_eq!(
                    LsRefsRequest::listed(&sorted, name),
                    has,
                    "{name} of {picked:?}"
                );
            }
        }
    }

    #[test]
    fn a_request_of_too_many_prefixes_is_one_for_every_ref() {
        let prefixes = |count: usize| (0..count).map(|n| format!("refs/heads/{n}"));
        let most = request(prefixes(MAX_PREFIXES - 1));
        assert_eq!(most.ref_prefixes.len(), MAX_PREFIXES - 1);
        assert!(!LsRefsRequest::listed(&most.prefixes(), "refs/tags/v1"));
        let over = request(prefixes(MAX_PREFIXES));
        assert!(over.ref_prefixes.is_empty());
        assert!(LsRefsRequest::listed(&over.prefixes(), "refs/tags/v1"));
    }
}
