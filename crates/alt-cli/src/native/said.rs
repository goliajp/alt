//! A refusal in git's own words.

use super::*;

/// The failure of a command that changed nothing and says `text` of it,
/// which is written as it is: the lines git writes, each with its own
/// `error:` or `fatal:`. The command ends with `code`.
pub(super) fn refused(text: &str, code: u8) -> Box<dyn std::error::Error> {
    if let Err(e) = crate::diagnostics::emit(text.as_bytes()) {
        return e.into();
    }
    let first = text.lines().next().unwrap_or_default();
    let first = first
        .strip_prefix("error: ")
        .or_else(|| first.strip_prefix("fatal: "))
        .unwrap_or(first);
    let refusal = crate::machine::Failure::refusal(crate::machine::REFUSED_AS_SAID, first);
    Box::new(crate::machine::Failure {
        exit_code: code,
        ..refusal
    })
}

/// git's refusal to write over what is not committed at `paths`, for the
/// command it calls `verb`, with `advice` on what to do first.
pub(super) fn would_overwrite(
    what: &str,
    paths: &[BString],
    verb: &str,
    advice: &str,
) -> Box<dyn std::error::Error> {
    let mut said = format!("error: {what} would be overwritten by {verb}:\n");
    for path in paths {
        said.push_str(&format!("\t{path}\n"));
    }
    said.push_str(advice);
    said.push_str("\nAborting\n");
    refused(&said, 1)
}
