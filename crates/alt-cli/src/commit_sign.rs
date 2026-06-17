//! M10/W15 — commit-level Ed25519 signing on the wire.
//!
//! A signed commit carries one extra header between `committer` and the
//! blank message-separator:
//!
//! ```text
//! alt-sig <principal> alt-sig-ed25519:<base64url>
//! ```
//!
//! The signature is over the *unsigned* commit bytes (the same object
//! minus the `alt-sig` header line). Verification reconstructs the
//! payload by stripping that single line — every other byte, including
//! ordering of the other headers, is preserved.
//!
//! Two operations:
//! - [`embed_alt_sig`] takes the unsigned commit bytes + a finished
//!   signature line and splices the line in just before the blank
//!   separator. The caller already has the line text (so callers in
//!   different signing flows can share the embed code).
//! - [`extract_alt_sig`] reverses the operation on the server: locates
//!   the `alt-sig` header line in the commit bytes, parses out the
//!   principal id and signature, and hands back the canonical (unsigned)
//!   payload the signature should verify against.
//!
//! The commit object's git compatibility is preserved either way:
//! git tolerates unknown headers; a signed commit is still a valid git
//! commit (parsable, hashable, fsck-clean), just with one alt-specific
//! header git ignores.

use alt_sign::Sig;

/// Build the byte form of an `alt-sig` header line, *including* the
/// trailing newline. Use this output as the `line` argument to
/// [`embed_alt_sig`].
pub fn alt_sig_line(principal: &str, sig: &Sig) -> Vec<u8> {
    // `Sig::to_text` always ends with '\n'; we strip it because the
    // header line constructor adds its own.
    let text = sig.to_text();
    let text = text.trim();
    format!("alt-sig {principal} {text}\n").into_bytes()
}

/// Insert `line` into the commit's header block, immediately before the
/// blank separator that terminates headers. Returns the new commit
/// bytes; the caller must rehash to get the new oid.
///
/// Returns `None` when `bytes` doesn't have a header/separator/body
/// shape (e.g. a non-commit object), which indicates a bug at the
/// caller rather than a recoverable input.
pub fn embed_alt_sig(bytes: &[u8], line: &[u8]) -> Option<Vec<u8>> {
    let separator = find_separator(bytes)?;
    let mut out = Vec::with_capacity(bytes.len() + line.len());
    out.extend_from_slice(&bytes[..separator]);
    out.extend_from_slice(line);
    out.extend_from_slice(&bytes[separator..]);
    Some(out)
}

/// Find and parse the `alt-sig` header in a commit object. Returns the
/// principal id, the parsed signature, and the canonical payload (the
/// commit bytes with the `alt-sig` header line removed) for verifying.
///
/// Returns `Ok(None)` when there is no `alt-sig` header — this is the
/// common path for unsigned commits and is not an error.
pub fn extract_alt_sig(bytes: &[u8]) -> Result<Option<AltSig>, AltSigError> {
    let separator = find_separator(bytes).ok_or(AltSigError::NotACommit)?;
    let header_block = &bytes[..separator];
    let Some(line_range) = locate_alt_sig_line(header_block) else {
        return Ok(None);
    };
    let line = &header_block[line_range.start..line_range.end];
    // strip the trailing '\n' we know is at line_range.end-1
    let line = if line.last() == Some(&b'\n') {
        &line[..line.len() - 1]
    } else {
        line
    };
    let rest = line
        .strip_prefix(b"alt-sig ")
        .ok_or(AltSigError::Malformed)?;
    let space = rest
        .iter()
        .position(|&b| b == b' ')
        .ok_or(AltSigError::Malformed)?;
    let principal_bytes = &rest[..space];
    let sig_text_bytes = &rest[space + 1..];
    let principal = std::str::from_utf8(principal_bytes).map_err(|_| AltSigError::Malformed)?;
    let sig_text = std::str::from_utf8(sig_text_bytes).map_err(|_| AltSigError::Malformed)?;
    let sig = Sig::from_text(sig_text).map_err(|_| AltSigError::Malformed)?;

    // Rebuild the canonical (unsigned) payload by deleting the alt-sig
    // line bytes from the original.
    let mut canonical = Vec::with_capacity(bytes.len() - (line_range.end - line_range.start));
    canonical.extend_from_slice(&bytes[..line_range.start]);
    canonical.extend_from_slice(&bytes[line_range.end..]);

    Ok(Some(AltSig {
        principal: principal.to_owned(),
        sig,
        canonical,
    }))
}

/// The verify-side parse result: the principal that claims authorship,
/// the parsed signature, and the canonical bytes the signature must
/// verify against.
#[derive(Debug, Clone)]
pub struct AltSig {
    pub principal: String,
    pub sig: Sig,
    pub canonical: Vec<u8>,
}

/// Failure to parse a commit's `alt-sig` header.
#[derive(Debug)]
pub enum AltSigError {
    NotACommit,
    Malformed,
}

impl std::fmt::Display for AltSigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotACommit => {
                f.write_str("object bytes don't have commit shape (no blank-line separator)")
            }
            Self::Malformed => f.write_str("alt-sig header present but malformed"),
        }
    }
}

impl std::error::Error for AltSigError {}

/// Locate the byte range of the `alt-sig` header line within the
/// header block (everything up to but not including the blank
/// separator). Returns the *line including its trailing newline*, so
/// the caller can slice the line out cleanly.
fn locate_alt_sig_line(header_block: &[u8]) -> Option<std::ops::Range<usize>> {
    // Header block is `name SP value LF` lines. `alt-sig` is fixed-name
    // single-line for now (M10/W15 doesn't define a continuation form),
    // so a direct line scan finds it.
    let mut start = 0;
    while start < header_block.len() {
        let end = header_block[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| start + i + 1)
            .unwrap_or(header_block.len());
        if header_block[start..end].starts_with(b"alt-sig ") {
            return Some(start..end);
        }
        start = end;
    }
    None
}

/// Find the offset of the blank line that separates the commit's
/// header block from its message body. The separator is a single LF
/// after a header LF — i.e. the byte position of the second LF in
/// `…LFLF…`. Returns the index of that separating LF.
fn find_separator(bytes: &[u8]) -> Option<usize> {
    // bytes shape: <headers ending in LF>LF<message>. Find first LFLF
    // and return the position of the inner LF (the one that terminates
    // the empty separator line).
    bytes.windows(2).position(|w| w == b"\n\n").map(|p| p + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_sign::SecretKey;

    const SAMPLE: &[u8] = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
        author A U Thor <a@example.com> 1700000000 +0900\n\
        committer A U Thor <a@example.com> 1700000001 +0900\n\
        \n\
        merge!\n";

    #[test]
    fn embed_then_extract_round_trips() {
        let (sec, pubk) = SecretKey::generate();
        let sig = sec.sign(SAMPLE);
        let line = alt_sig_line("alice", &sig);
        let signed = embed_alt_sig(SAMPLE, &line).expect("embed");
        // Embedded form has the new header line spliced before the
        // blank separator.
        assert!(signed.windows(8).any(|w| w == b"alt-sig "));
        // Extract recovers principal, signature, and the original
        // canonical (unsigned) payload.
        let parsed = extract_alt_sig(&signed).unwrap().expect("alt-sig present");
        assert_eq!(parsed.principal, "alice");
        assert_eq!(parsed.canonical, SAMPLE);
        // Signature verifies against the canonical payload.
        pubk.verify(&parsed.canonical, &parsed.sig).unwrap();
    }

    #[test]
    fn extract_returns_none_when_unsigned() {
        let parsed = extract_alt_sig(SAMPLE).unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn extract_returns_err_on_non_commit_bytes() {
        let err = extract_alt_sig(b"not a commit").unwrap_err();
        assert!(matches!(err, AltSigError::NotACommit));
    }
}
