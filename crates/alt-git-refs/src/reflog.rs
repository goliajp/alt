//! The log git keeps of a ref: a line for each value the ref was given,
//! the oldest first, with the value before it, who gave it and when,
//! and what was done.

use alt_git_codec::{HashAlgo, ObjectId};

/// One line of a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// what the ref named before; `None` where it named nothing
    pub old: Option<ObjectId>,
    /// what it names from here on; `None` where it was taken away
    pub new: Option<ObjectId>,
    /// `name <email> seconds zone`, as git writes who moved the ref
    pub by: Vec<u8>,
    /// what was done; `None` for a line that ends after who and when
    pub said: Option<Vec<u8>>,
}

/// The id `hex` spells, `None` inside for the id of all zeros.
fn id(hex: &[u8]) -> Option<Option<ObjectId>> {
    if hex.iter().all(|b| *b == b'0') && matches!(hex.len(), 40 | 64) {
        return Some(None);
    }
    ObjectId::from_hex(hex).ok().map(Some)
}

/// The lines of `log`, the oldest first. What is no line of a log is
/// passed over, as git passes it over.
pub fn parse(log: &[u8]) -> Vec<Line> {
    let mut lines = Vec::new();
    for line in log.split(|b| *b == b'\n') {
        let (head, said) = match line.iter().position(|b| *b == b'\t') {
            Some(tab) => (&line[..tab], Some(line[tab + 1..].to_vec())),
            None => (line, None),
        };
        let mut parts = head.splitn(3, |b| *b == b' ');
        let (Some(old), Some(new), Some(by)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (Some(old), Some(new)) = (id(old), id(new)) else {
            continue;
        };
        lines.push(Line {
            old,
            new,
            by: by.to_vec(),
            said,
        });
    }
    lines
}

/// `lines` as git writes them for ids of `algo`.
pub fn write(lines: &[Line], algo: HashAlgo) -> Vec<u8> {
    let zero = "0".repeat(algo.hex_len());
    let hex = |id: &Option<ObjectId>| id.map_or_else(|| zero.clone(), |id| id.to_string());
    let mut log = Vec::new();
    for line in lines {
        log.extend_from_slice(format!("{} {} ", hex(&line.old), hex(&line.new)).as_bytes());
        log.extend_from_slice(&line.by);
        if let Some(said) = &line.said {
            log.push(b'\t');
            log.extend_from_slice(said);
        }
        log.push(b'\n');
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_is_read_line_by_line_and_written_back_as_it_was() {
        let (zero, one, two) = ("0".repeat(40), "01".repeat(20), "02".repeat(20));
        let log = format!(
            "{zero} {one} A U Thor <a@x> 1700000000 +0900\tcommit (initial): base\n\
             {one} {two} A U Thor <a@x> 1700000001 +0900\tcheckout: moving from a to b\n\
             {two} {zero} A U Thor <a@x> 1700000002 +0900\n"
        );
        let lines = parse(log.as_bytes());
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].old, None);
        assert_eq!(lines[0].new, Some(ObjectId::Sha1([1; 20])));
        assert_eq!(lines[0].by, b"A U Thor <a@x> 1700000000 +0900");
        assert_eq!(
            lines[1].said.as_deref(),
            Some(&b"checkout: moving from a to b"[..])
        );
        assert_eq!(lines[2].said, None);
        assert_eq!(lines[2].new, None);
        assert_eq!(write(&lines, HashAlgo::Sha1), log.as_bytes());
        assert!(parse(b"not a line of a log\n\n").is_empty());
    }
}
