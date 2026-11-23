//! Writing one variable into the text of a configuration file as
//! `git config` writes it: every other byte of the file stays.

use alt_bytes::BString;

use crate::{ConfigError, Entry, parse};

/// A variable by its full name: the section in lower case, the
/// subsection as written, the name in lower case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Name<'a> {
    pub section: &'a str,
    pub subsection: Option<&'a [u8]>,
    pub key: &'a str,
}

impl<'a> Name<'a> {
    pub fn of(entry: &'a Entry) -> Self {
        Name {
            section: &entry.section,
            subsection: entry.subsection.as_ref().map(|sub| sub.as_slice()),
            key: &entry.key,
        }
    }

    fn names(&self, entry: &Entry) -> bool {
        Name::of(entry) == *self
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Blank,
    Comment,
    /// a section header, and whether it is the section of the variable
    Section(bool),
    Entry,
}

/// A stretch of the file: git's parser hands the file to a writer cut
/// into these, each reaching up to where the next begins.
struct Piece {
    kind: Kind,
    begin: usize,
    end: usize,
}

/// The text of `file` with `name` set to `value`, as `git config <name>
/// <value>` leaves it: the one line that sets the variable is replaced,
/// or a line follows the last variable of its section, or a section for
/// it ends the file. A variable the file sets more than once is not set.
pub fn set(file: &[u8], name: Name<'_>, value: &[u8]) -> Result<Vec<u8>, ConfigError> {
    write(file, name, value, true)
}

/// The text of `file` with one more `value` for `name`, as `git config
/// --add <name> <value>` leaves it.
pub fn add(file: &[u8], name: Name<'_>, value: &[u8]) -> Result<Vec<u8>, ConfigError> {
    write(file, name, value, false)
}

fn write(file: &[u8], name: Name<'_>, value: &[u8], replace: bool) -> Result<Vec<u8>, ConfigError> {
    let entries = parse::parse_file(file)?;
    let pieces = pieces(file, name)?;
    // the piece the new line goes behind or in place of, as git finds
    // it: the last header or variable of the section while the variable
    // itself was not seen, then each line that sets it
    let (mut at, mut found, mut section_seen) = (None, 0, false);
    let (mut in_section, mut nth) = (false, 0);
    for (index, piece) in pieces.iter().enumerate() {
        match piece.kind {
            Kind::Section(ours) => {
                in_section = ours;
                if ours && found == 0 {
                    (at, section_seen) = (Some(index), true);
                }
            }
            Kind::Entry => {
                let named = replace && name.names(&entries[nth]);
                nth += 1;
                if found > 0 && named {
                    return Err(ConfigError::Value("variable has multiple values"));
                }
                if found == 0 && in_section {
                    (at, section_seen) = (Some(index), true);
                    found += usize::from(named);
                }
            }
            Kind::Blank | Kind::Comment => {}
        }
    }
    let at = at.or(pieces.len().checked_sub(1));
    let (mut copy_end, replace_end) = match at.map(|index| &pieces[index]) {
        None => (0, 0),
        Some(piece) if found == 0 => {
            let mut end = piece.end;
            // the newline that ends a section header stays with it
            if end > 0 && end < file.len() && file[end - 1] != b'\n' && file[end] == b'\n' {
                end += 1;
            }
            (end, end)
        }
        Some(piece) => (piece.begin, piece.end),
    };
    if found > 0 {
        let blank = |b: u8| matches!(b, b' ' | b'\t' | b'\r');
        while copy_end > 0 && blank(file[copy_end - 1]) {
            copy_end -= 1;
        }
    }
    let mut out = Vec::with_capacity(file.len() + value.len() + 32);
    if copy_end > 0 {
        out.extend_from_slice(&file[..copy_end]);
        if file[copy_end - 1] != b'\n' {
            out.push(b'\n');
        }
    }
    if !section_seen {
        header(&mut out, name);
    }
    pair(&mut out, name.key, value);
    out.extend_from_slice(&file[replace_end..]);
    Ok(out)
}

fn header(out: &mut Vec<u8>, name: Name<'_>) {
    out.push(b'[');
    out.extend_from_slice(name.section.as_bytes());
    if let Some(sub) = name.subsection {
        out.extend_from_slice(b" \"");
        for &b in sub {
            if b == b'"' || b == b'\\' {
                out.push(b'\\');
            }
            out.push(b);
        }
        out.push(b'"');
    }
    out.extend_from_slice(b"]\n");
}

fn pair(out: &mut Vec<u8>, key: &str, value: &[u8]) {
    // quotes keep what a reader would drop: a blank at either end, and
    // what follows a character that begins a comment
    let quoted = value.first() == Some(&b' ')
        || value.last() == Some(&b' ')
        || value.iter().any(|b| matches!(b, b';' | b'#' | b'\r'));
    out.push(b'\t');
    out.extend_from_slice(key.as_bytes());
    out.extend_from_slice(b" = ");
    if quoted {
        out.push(b'"');
    }
    for &b in value {
        match b {
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'"' | b'\\' => out.extend_from_slice(&[b'\\', b]),
            _ => out.push(b),
        }
    }
    if quoted {
        out.push(b'"');
    }
    out.push(b'\n');
}

/// Cuts a file that parses into its pieces.
fn pieces(file: &[u8], name: Name<'_>) -> Result<Vec<Piece>, ConfigError> {
    let mut pieces: Vec<Piece> = Vec::new();
    let mut pos = match file.starts_with(b"\xef\xbb\xbf") {
        true => 3,
        false => 0,
    };
    while pos < file.len() {
        let begin = pos;
        let kind = match file[pos] {
            b'\n' | b' ' | b'\t' | b'\r' => {
                pos += 1;
                Kind::Blank
            }
            b'#' | b';' => {
                pos = line_end(file, pos);
                Kind::Comment
            }
            b'[' => {
                pos = header_end(file, pos);
                let (section, subsection) = parse::section_of(&file[begin..pos])?;
                let subsection: Option<&[u8]> = subsection.as_ref().map(|sub| sub.as_slice());
                Kind::Section(section == name.section && subsection == name.subsection)
            }
            _ => {
                pos = entry_end(file, pos);
                Kind::Entry
            }
        };
        match pieces.last_mut() {
            Some(last) if kind == Kind::Blank && last.kind == Kind::Blank => last.end = pos,
            _ => pieces.push(Piece {
                kind,
                begin,
                end: pos,
            }),
        }
    }
    Ok(pieces)
}

/// Where the line `pos` is on ends, before its newline.
fn line_end(file: &[u8], pos: usize) -> usize {
    let rest = &file[pos..];
    pos + rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len())
}

/// Behind the `]` of the section header that begins at `pos`.
fn header_end(file: &[u8], mut pos: usize) -> usize {
    let mut quoted = false;
    while pos < file.len() {
        match file[pos] {
            b'\\' if quoted => pos += 1,
            b'"' => quoted = !quoted,
            b']' if !quoted => return pos + 1,
            _ => {}
        }
        pos += 1;
    }
    file.len()
}

/// Behind the newline that ends the variable that begins at `pos`, its
/// continuation lines and its comment with it.
fn entry_end(file: &[u8], mut pos: usize) -> usize {
    let (mut quoted, mut comment) = (false, false);
    while pos < file.len() {
        match file[pos] {
            b'\n' => return pos + 1,
            _ if comment => {}
            b'\\' => {
                pos += 1;
                // a backslash before CR LF continues the line as well
                if file.get(pos) == Some(&b'\r') && file.get(pos + 1) == Some(&b'\n') {
                    pos += 1;
                }
            }
            b'"' => quoted = !quoted,
            b'#' | b';' if !quoted => comment = true,
            _ => {}
        }
        pos += 1;
    }
    file.len()
}

/// `source` with the variables `own` sets written into it, each as `git
/// config` would write it: a variable `own` gives one value and `source`
/// at most one is set, every other is added value by value behind what
/// `source` has, and one that reads the same in both is left alone.
/// A variable without a value is written as `true`.
pub fn overlay(source: &[u8], own: &[Entry]) -> Result<Vec<u8>, ConfigError> {
    let mut out = source.to_vec();
    let mut done: Vec<Name<'_>> = Vec::new();
    for entry in own {
        let name = Name::of(entry);
        if done.contains(&name) {
            continue;
        }
        done.push(name);
        let values = |entries: &[Entry]| -> Vec<Option<BString>> {
            let named = entries.iter().filter(|e| name.names(e));
            named.map(|e| e.value.clone()).collect()
        };
        let (ours, theirs) = (values(own), values(&parse::parse_file(&out)?));
        if ours == theirs {
            continue;
        }
        let text = |value: &Option<BString>| match value {
            Some(value) => value.to_vec(),
            None => b"true".to_vec(),
        };
        match (ours.as_slice(), theirs.len()) {
            ([one], 0 | 1) => out = set(&out, name, &text(one))?,
            _ => {
                for value in &ours {
                    out = add(&out, name, &text(value))?;
                }
            }
        }
    }
    Ok(out)
}
