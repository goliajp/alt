//! How far down its history a fetch asks to be given: the lines of a
//! shallow fetch, and the sections a server answers them and the `have`
//! lines with.

use std::io::Write;

use alt_git_codec::{HashAlgo, ObjectId};

use super::{FetchError, ShallowInfo, parse_oid};
use crate::pkt;

/// The most `shallow` lines of a request: one for each commit its sender
/// holds without its parents.
pub const MAX_SHALLOWS: usize = 1 << 18;
/// The most `deepen-not` lines of a request.
pub const MAX_DEEPEN_NOTS: usize = 1 << 10;
/// The depth that asks for the whole of a history.
pub const WHOLE_DEPTH: u32 = 0x7fff_ffff;

/// The lines of a request that say where its history ends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Depth {
    /// `shallow <oid>`: a commit its sender holds without its parents
    pub shallows: Vec<ObjectId>,
    /// `deepen <n>`: no more than this many commits down
    pub deepen: Option<u32>,
    /// `deepen-relative`: the depth is counted from `shallows`
    pub relative: bool,
    /// `deepen-since <time>`: no commit older than this
    pub since: Option<i64>,
    /// `deepen-not <ref>`: nothing that these refs reach
    pub not: Vec<String>,
}

impl Depth {
    /// Whether the request asks for an end to its history, and so for a
    /// `shallow-info` section.
    pub fn asked(&self) -> bool {
        self.deepen.is_some() || self.since.is_some() || !self.not.is_empty()
    }

    /// Takes `line` of the arguments where it is one of these. Whether it
    /// was.
    pub(super) fn line(&mut self, s: &str, line: &[u8]) -> Result<bool, FetchError> {
        let bad = || FetchError::BadLine {
            section: "args".into(),
            line: line.to_vec(),
        };
        if let Some(hex) = s.strip_prefix("shallow ") {
            if self.shallows.len() >= MAX_SHALLOWS {
                return Err(FetchError::TooMany("shallow"));
            }
            let oid = parse_oid(hex.trim(), HashAlgo::Sha1, line, "args")?;
            self.shallows.push(oid);
        } else if let Some(depth) = s.strip_prefix("deepen ") {
            let depth = depth.parse::<u32>().ok().filter(|depth| *depth > 0);
            self.deepen = Some(depth.ok_or_else(bad)?.min(WHOLE_DEPTH));
        } else if s == "deepen-relative" {
            self.relative = true;
        } else if let Some(time) = s.strip_prefix("deepen-since ") {
            let time = time.parse::<i64>().ok().filter(|time| *time > 0);
            self.since = Some(time.ok_or_else(bad)?);
        } else if let Some(name) = s.strip_prefix("deepen-not ") {
            if self.not.len() >= MAX_DEEPEN_NOTS {
                return Err(FetchError::TooMany("deepen-not"));
            }
            self.not.push(name.to_owned());
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}

pub(super) fn parse_shallow(line: &[u8], algo: HashAlgo) -> Result<ShallowInfo, FetchError> {
    let bad = || FetchError::BadLine {
        section: "shallow-info".into(),
        line: line.to_vec(),
    };
    let s = std::str::from_utf8(line).map_err(|_| bad())?;
    if let Some(rest) = s.strip_prefix("shallow ") {
        let oid = parse_oid(rest.trim(), algo, line, "shallow-info")?;
        return Ok(ShallowInfo::Shallow(oid));
    }
    if let Some(rest) = s.strip_prefix("unshallow ") {
        let oid = parse_oid(rest.trim(), algo, line, "shallow-info")?;
        return Ok(ShallowInfo::Unshallow(oid));
    }
    Err(bad())
}

/// The `acknowledgments` section of a response to a request that did not
/// say `done`: the commits of its `have` lines that the server has too.
/// With `ready` the rest of the response follows; without, the response
/// is over and its sender goes on with further `have` lines.
pub fn encode_acknowledgments<W: Write>(
    w: &mut W,
    common: &[ObjectId],
    ready: bool,
) -> std::io::Result<()> {
    pkt::write_data(w, b"acknowledgments\n")?;
    if common.is_empty() {
        pkt::write_data(w, b"NAK\n")?;
    }
    for oid in common {
        pkt::write_data(w, format!("ACK {oid}\n").as_bytes())?;
    }
    match ready {
        true => {
            pkt::write_data(w, b"ready\n")?;
            pkt::write_delim(w)
        }
        false => pkt::write_flush(w),
    }
}

/// The `shallow-info` section of a response: where the history given ends
/// and where it no longer does.
pub fn encode_shallow_info<W: Write>(w: &mut W, info: &[ShallowInfo]) -> std::io::Result<()> {
    pkt::write_data(w, b"shallow-info\n")?;
    for line in info {
        let line = match line {
            ShallowInfo::Shallow(oid) => format!("shallow {oid}\n"),
            ShallowInfo::Unshallow(oid) => format!("unshallow {oid}\n"),
        };
        pkt::write_data(w, line.as_bytes())?;
    }
    pkt::write_delim(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{FetchAck, parse_fetch_request, read_fetch_preamble};
    use crate::pkt::{write_data, write_delim, write_flush};

    fn request(lines: &[String]) -> Vec<u8> {
        let mut body = Vec::new();
        write_data(&mut body, b"command=fetch\n").unwrap();
        write_delim(&mut body).unwrap();
        for line in lines {
            write_data(&mut body, line.as_bytes()).unwrap();
        }
        write_flush(&mut body).unwrap();
        body
    }

    fn id(n: usize) -> ObjectId {
        format!("{n:040x}").parse().unwrap()
    }

    #[test]
    fn the_lines_of_a_shallow_fetch_are_in_the_request_read() {
        let lines = [
            format!("want {}\n", id(1)),
            format!("shallow {}\n", id(2)),
            format!("shallow {}\n", id(3)),
            "deepen 3\n".to_owned(),
            "deepen-relative\n".to_owned(),
            "wait-for-done\n".to_owned(),
        ];
        let (read, _) = parse_fetch_request(&mut &request(&lines)[..]).unwrap();
        assert_eq!(read.depth.shallows, [id(2), id(3)]);
        assert_eq!(read.depth.deepen, Some(3));
        assert!(read.depth.relative && read.depth.asked() && read.wait_for_done);
        assert!(!read.done);

        let lines = [
            "deepen-since 1700000000\n".to_owned(),
            "deepen-not refs/heads/old\n".to_owned(),
            "deepen-not v1\n".to_owned(),
        ];
        let (read, _) = parse_fetch_request(&mut &request(&lines)[..]).unwrap();
        assert_eq!(read.depth.since, Some(1_700_000_000));
        assert_eq!(read.depth.not, ["refs/heads/old", "v1"]);
        assert!(read.depth.asked() && read.depth.deepen.is_none());

        let plain = [format!("shallow {}\n", id(2))];
        let (read, _) = parse_fetch_request(&mut &request(&plain)[..]).unwrap();
        assert!(!read.depth.asked());
        let whole = ["deepen 4294967295\n".to_owned()];
        let (read, _) = parse_fetch_request(&mut &request(&whole)[..]).unwrap();
        assert_eq!(read.depth.deepen, Some(WHOLE_DEPTH));
    }

    #[test]
    fn a_depth_that_is_no_number_above_zero_is_refused_and_so_are_too_many_lines() {
        for line in ["deepen 0", "deepen -1", "deepen x", "deepen ", "deepen 1 2"] {
            let read = parse_fetch_request(&mut &request(&[format!("{line}\n")])[..]);
            assert!(matches!(read, Err(FetchError::BadLine { .. })), "{line}");
        }
        for line in ["deepen-since 0", "deepen-since -5", "deepen-since soon"] {
            let read = parse_fetch_request(&mut &request(&[format!("{line}\n")])[..]);
            assert!(matches!(read, Err(FetchError::BadLine { .. })), "{line}");
        }
        let shallows = |count: usize| vec![format!("shallow {}\n", id(1)); count];
        assert!(parse_fetch_request(&mut &request(&shallows(MAX_SHALLOWS))[..]).is_ok());
        let over = parse_fetch_request(&mut &request(&shallows(MAX_SHALLOWS + 1))[..]);
        assert!(matches!(over, Err(FetchError::TooMany("shallow"))));
        let nots = |count: usize| vec!["deepen-not a\n".to_owned(); count];
        assert!(parse_fetch_request(&mut &request(&nots(MAX_DEEPEN_NOTS))[..]).is_ok());
        let over = parse_fetch_request(&mut &request(&nots(MAX_DEEPEN_NOTS + 1))[..]);
        assert!(matches!(over, Err(FetchError::TooMany("deepen-not"))));
    }

    #[test]
    fn the_sections_before_a_pack_are_read_back_as_they_were_written() {
        let mut out = Vec::new();
        encode_acknowledgments(&mut out, &[id(5), id(6)], true).unwrap();
        let info = [ShallowInfo::Shallow(id(7)), ShallowInfo::Unshallow(id(8))];
        encode_shallow_info(&mut out, &info).unwrap();
        write_data(&mut out, b"packfile\n").unwrap();
        let read = read_fetch_preamble(&mut &out[..], HashAlgo::Sha1).unwrap();
        let acks = [FetchAck::Ack(id(5)), FetchAck::Ack(id(6)), FetchAck::Ready];
        assert_eq!(read.acknowledgments, acks);
        assert_eq!(read.shallow_info, info);
        assert!(!read.packfile_missing);

        let mut out = Vec::new();
        encode_acknowledgments(&mut out, &[], false).unwrap();
        let read = read_fetch_preamble(&mut &out[..], HashAlgo::Sha1).unwrap();
        assert_eq!(read.acknowledgments, [FetchAck::Nak]);
        assert!(read.packfile_missing);
    }
}
