//! What `git fsck` says of the blob a tree names `.gitmodules` or
//! `.gitattributes`.

use crate::Special;

/// The largest `.gitmodules` read: Git reads no larger blob of a pack whole
/// (`core.bigFileThreshold`), and refuses the file it cannot read.
pub const MODULES_LARGEST: u64 = 512 << 20;
/// the largest `.gitattributes` Git reads
pub const ATTRIBUTES_LARGEST: u64 = 100 << 20;
/// the longest line of `.gitattributes` Git reads, and one byte more
const ATTRIBUTES_LINE: usize = 2048;

/// What the object a [`crate::File`] names is.
pub enum Blob<'a> {
    /// no object of the repository
    Missing,
    /// another kind of object
    NotBlob,
    /// a blob larger than the file is read up to
    Larger,
    Bytes(&'a [u8]),
}

/// What `git fsck` refuses the object `blob` for, where a tree names it as
/// `special`, in its words.
pub fn blob_fault(special: Special, blob: Blob<'_>) -> Option<&'static str> {
    match (special, blob) {
        (Special::Gitmodules, Blob::Missing) => {
            Some("gitmodulesMissing: unable to read .gitmodules blob")
        }
        (Special::Gitmodules, Blob::NotBlob) => {
            Some("gitmodulesBlob: non-blob found at .gitmodules")
        }
        (Special::Gitmodules, Blob::Larger) => {
            Some("gitmodulesLarge: .gitmodules too large to parse")
        }
        (Special::Gitmodules, Blob::Bytes(data)) => modules(data),
        (Special::Gitattributes, Blob::Missing) => {
            Some("gitattributesMissing: unable to read .gitattributes blob")
        }
        (Special::Gitattributes, Blob::NotBlob) => {
            Some("gitattributesBlob: non-blob found at .gitattributes")
        }
        (Special::Gitattributes, Blob::Larger) => {
            Some("gitattributesLarge: .gitattributes too large to parse")
        }
        (Special::Gitattributes, Blob::Bytes(data)) => attributes(data),
    }
}

/// The settings of submodules Git refuses: a name that climbs out of the
/// directory of submodules, a URL a transport would take for an option or
/// would be fooled by, a path that is an option, an update that is a
/// command. What follows the first line Git cannot read is not read: a file
/// Git cannot read at all is no fault.
fn modules(data: &[u8]) -> Option<&'static str> {
    let (entries, _) = alt_git_config::parse_until_wrong(data);
    for entry in entries {
        let (true, Some(name)) = (entry.section == "submodule", entry.subsection.as_deref()) else {
            continue;
        };
        if !name_is_safe(name) {
            return Some("gitmodulesName: disallowed submodule name");
        }
        let value = entry.value.as_deref().map(|value| &value[..]);
        match (entry.key.as_str(), value) {
            ("url", Some(url)) if !crate::url::submodule_url_is_safe(url) => {
                return Some("gitmodulesUrl: disallowed submodule url");
            }
            ("path", Some(path)) if path.starts_with(b"-") => {
                return Some("gitmodulesPath: disallowed submodule path");
            }
            ("update", Some(update)) if update.starts_with(b"!") => {
                return Some("gitmodulesUpdate: disallowed submodule update setting");
            }
            _ => {}
        }
    }
    None
}

/// a name of a submodule Git takes: not empty, and no part of it `..`
fn name_is_safe(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .split(|byte| matches!(byte, b'/' | b'\\'))
            .all(|part| part != b"..")
}

/// Lines no longer than Git reads, up to the first NUL, which Git reads as
/// the end.
fn attributes(data: &[u8]) -> Option<&'static str> {
    if data.len() as u64 > ATTRIBUTES_LARGEST {
        return Some("gitattributesLarge: .gitattributes too large to parse");
    }
    let end = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(data.len());
    let long = data[..end].split(|byte| *byte == b'\n');
    long.into_iter()
        .any(|line| line.len() >= ATTRIBUTES_LINE)
        .then_some("gitattributesLineLength: .gitattributes has too long lines to parse")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(data: &str) -> Option<&'static str> {
        blob_fault(Special::Gitmodules, Blob::Bytes(data.as_bytes()))
    }

    #[test]
    fn settings_of_submodules_git_refuses_are_refused() {
        assert_eq!(
            said("[submodule \"a\"]\n\tpath = a\n\turl = ../a.git\n"),
            None
        );
        let name = Some("gitmodulesName: disallowed submodule name");
        assert_eq!(said("[submodule \"../x\"]\n\tpath = a\n"), name);
        assert_eq!(said("[submodule \"a\\\\..\"]\n\tpath = a\n"), name);
        assert_eq!(said("[submodule \"\"]\n\tpath = a\n"), name);
        assert_eq!(said("[submodule \"a..b\"]\n\tpath = a\n"), None);
        let url = Some("gitmodulesUrl: disallowed submodule url");
        assert_eq!(said("[submodule \"a\"]\n\turl = -upload-pack=x\n"), url);
        assert_eq!(said("[submodule \"a\"]\n\turl = ./%0ax\n"), url);
        assert_eq!(said("[submodule \"a\"]\n\turl = ../../:x\n"), url);
        assert_eq!(said("[submodule \"a\"]\n\turl = https://h/%0a\n"), url);
        assert_eq!(said("[submodule \"a\"]\n\turl = https:///x\n"), url);
        assert_eq!(said("[submodule \"a\"]\n\turl = git://h/x\n"), None);
        assert_eq!(
            said("[submodule \"a\"]\n\tpath = -x\n"),
            Some("gitmodulesPath: disallowed submodule path")
        );
        assert_eq!(
            said("[submodule \"a\"]\n\tupdate = !rm\n"),
            Some("gitmodulesUpdate: disallowed submodule update setting")
        );
        // what comes before a line that is not read is read; nothing after it
        assert_eq!(
            said("[submodule \"a\"]\n\tpath = -x\n[[\n"),
            said("[submodule \"a\"]\n\tpath = -x\n")
        );
        assert_eq!(said("[[\n[submodule \"a\"]\n\tpath = -x\n"), None);
        // other sections, and the section without a subsection
        assert_eq!(said("[core]\n\turl = -x\n[submodule]\n\tpath = -x\n"), None);
    }

    #[test]
    fn attributes_git_cannot_read_are_refused() {
        let attributes = |data: &[u8]| blob_fault(Special::Gitattributes, Blob::Bytes(data));
        assert_eq!(attributes(b"*.c diff\n"), None);
        let long = [vec![b'x'; 2047], b"\n".to_vec(), vec![b'y'; 2048]].concat();
        assert_eq!(
            attributes(&long),
            Some("gitattributesLineLength: .gitattributes has too long lines to parse")
        );
        assert_eq!(attributes(&long[..2047]), None);
        // Git reads up to a NUL
        let cut = [b"a\0".to_vec(), vec![b'y'; 4096]].concat();
        assert_eq!(attributes(&cut), None);
        assert_eq!(
            blob_fault(Special::Gitattributes, Blob::NotBlob),
            Some("gitattributesBlob: non-blob found at .gitattributes")
        );
    }
}
