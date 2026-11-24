//! Taking a variable or a section out of the text of a configuration
//! file, and giving a section another name, as `git config --unset-all`,
//! `--remove-section` and `--rename-section` leave the file.

use super::{Kind, Name, Piece, header, header_end, pieces};
use crate::{ConfigError, parse};

/// The text of `file` without the lines that set `name`; `None` where
/// none does. A section the variable was all of goes with it, unless a
/// comment stands in or before it.
pub fn unset(file: &[u8], name: Name<'_>) -> Result<Option<Vec<u8>>, ConfigError> {
    let entries = parse::parse_file(file)?;
    let pieces = pieces(file, name)?;
    // whether each piece is of the section of the variable
    let mut ours = Vec::with_capacity(pieces.len());
    let (mut in_section, mut nth, mut seen) = (false, 0, Vec::new());
    for (index, piece) in pieces.iter().enumerate() {
        match piece.kind {
            Kind::Section(named) => in_section = named,
            Kind::Entry => {
                if name.names(&entries[nth]) {
                    seen.push(index);
                }
                nth += 1;
            }
            Kind::Blank | Kind::Comment => {}
        }
        ours.push(in_section);
    }
    if seen.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(file.len());
    let mut copy_begin = 0;
    for at in seen {
        let (mut copy_end, replace_end) = match whole_section(&pieces, &ours, at) {
            Some(section) => section,
            None => (pieces[at].begin, pieces[at].end),
        };
        let blank = |b: u8| matches!(b, b' ' | b'\t' | b'\r');
        while copy_end > 0 && blank(file[copy_end - 1]) {
            copy_end -= 1;
        }
        if copy_end > copy_begin {
            out.extend_from_slice(&file[copy_begin..copy_end]);
            if file[copy_end - 1] != b'\n' {
                out.push(b'\n');
            }
        }
        copy_begin = replace_end.max(copy_begin);
    }
    out.extend_from_slice(&file[copy_begin..]);
    Ok(Some(out))
}

/// The stretch of the file that is the section of the entry `at` where
/// that entry is all the section sets and no comment stands before the
/// entry, before the header or behind the entry in the section.
fn whole_section(pieces: &[Piece], ours: &[bool], at: usize) -> Option<(usize, usize)> {
    // from behind what is of another section: the blank lines before
    // the header go with it
    let (mut begin, mut headed) = (pieces[at].begin, false);
    for index in (0..at).rev() {
        match pieces[index].kind {
            Kind::Comment => return None,
            Kind::Entry if ours[index] => return None,
            Kind::Entry | Kind::Section(false) => break,
            Kind::Section(true) => headed = true,
            Kind::Blank => {}
        }
        begin = pieces[index].begin;
    }
    let mut end = pieces.last()?.end;
    for index in at + 1..pieces.len() {
        match pieces[index].kind {
            Kind::Comment => return None,
            Kind::Entry if ours[index] => return None,
            Kind::Section(true) | Kind::Blank => continue,
            Kind::Entry | Kind::Section(false) => {}
        }
        end = pieces[index].begin;
        break;
    }
    headed.then_some((begin, end))
}

/// The lines of `file`, each with its newline.
fn lines(file: &[u8]) -> impl Iterator<Item = &[u8]> {
    file.split_inclusive(|b| *b == b'\n')
}

/// Where the header ends that begins the line, when the line begins
/// with the header of `[<section> "<subsection>"]`.
fn names_section(
    line: &[u8],
    section: &str,
    subsection: &[u8],
) -> Result<Option<usize>, ConfigError> {
    let begin = line.iter().take_while(|b| b.is_ascii_whitespace()).count();
    if line.get(begin) != Some(&b'[') {
        return Ok(None);
    }
    let end = header_end(line, begin);
    let (_, sub) = parse::section_of(&line[begin..end])?;
    // the name of the section as it is written: git tells its case here
    let written = &line[begin + 1..end];
    let written = written.split(|b| matches!(b, b' ' | b'\t' | b']')).next();
    let named = written == Some(section.as_bytes())
        && sub.as_ref().map(|s| s.as_slice()) == Some(subsection);
    Ok(named.then_some(end))
}

fn begins_section(line: &[u8]) -> bool {
    let begin = line.iter().take_while(|b| b.is_ascii_whitespace()).count();
    line.get(begin) == Some(&b'[')
}

/// The text of `file` without the sections `[<section> "<subsection>"]`,
/// each from its header to the next header; `None` where there is none.
pub fn remove_section(
    file: &[u8],
    section: &str,
    subsection: &[u8],
) -> Result<Option<Vec<u8>>, ConfigError> {
    parse::parse_file(file)?;
    let (mut out, mut removing, mut found) = (Vec::with_capacity(file.len()), false, false);
    for line in lines(file) {
        if begins_section(line) {
            removing = names_section(line, section, subsection)?.is_some();
            found |= removing;
        }
        if !removing {
            out.extend_from_slice(line);
        }
    }
    Ok(found.then_some(out))
}

/// The text of `file` with each header `[<section> "<from>"]` written as
/// `[<section> "<to>"]`; `None` where there is none. What stood behind a
/// header on its line follows the new header on a line of its own.
pub fn rename_section(
    file: &[u8],
    section: &str,
    from: &[u8],
    to: &[u8],
) -> Result<Option<Vec<u8>>, ConfigError> {
    parse::parse_file(file)?;
    let (mut out, mut found) = (Vec::with_capacity(file.len()), false);
    for line in lines(file) {
        let Some(end) = names_section(line, section, from)? else {
            out.extend_from_slice(line);
            continue;
        };
        found = true;
        let renamed = Name {
            section,
            subsection: Some(to),
            key: "",
        };
        header(&mut out, renamed);
        let rest = &line[end..];
        let blank = rest.iter().take_while(|b| b.is_ascii_whitespace()).count();
        if blank < rest.len() {
            out.push(b'\t');
            out.extend_from_slice(&rest[blank..]);
        }
    }
    Ok(found.then_some(out))
}
