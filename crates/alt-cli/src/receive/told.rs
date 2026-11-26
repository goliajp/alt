//! What a pusher is told of an error of the server in taking its push in.

/// What a pusher is told of an error of the server at a step of taking its
/// push in, the step being named: a server tells some no more than the
/// step and the kind of the error.
pub type Told = dyn Fn(&str, &(dyn std::error::Error + 'static)) -> String;

/// [`Told`] in whole: the step and the error.
pub fn told_whole(step: &str, error: &(dyn std::error::Error + 'static)) -> String {
    format!("{step}: {error}")
}

/// What a pusher is told of `error` in indexing or writing its pack: what
/// is wrong with the pack it sent in whole, and an error of the server as
/// `told` has it.
pub(super) fn unpack_told(told: &Told, error: &(dyn std::error::Error + 'static)) -> String {
    use alt_git_pack::PackError;
    match error.downcast_ref::<PackError>() {
        Some(PackError::Format(_) | PackError::Limit(_)) => format!("index-pack: {error}"),
        _ => told("index-pack", error),
    }
}

/// What a pusher is told of `error` of the transaction of its refs: why
/// it is refused in whole, and an error of the server as `told` has it.
pub(super) fn ref_tx_told(told: &Told, error: &alt_refs::RefError) -> String {
    use alt_refs::RefError;
    match error {
        RefError::Io(_) | RefError::OpLog(_) | RefError::Recorded { .. } => told("ref tx", error),
        _ => format!("ref tx: {error}"),
    }
}

/// Where a debug build is told to, the pack of a push fails to be indexed
/// as one that an error of the server stops: with
/// `ALT_SERVER_INDEX_FAILS=<file>`, with an error that names the file, which
/// is not there. A release build holds none of this.
#[cfg(debug_assertions)]
pub(super) fn failing() -> std::io::Result<()> {
    let Some(file) = std::env::var_os("ALT_SERVER_INDEX_FAILS") else {
        return Ok(());
    };
    let file = std::path::PathBuf::from(file);
    let gone = std::fs::File::open(&file).expect_err("the file is not there");
    let named = format!("{}: {gone}", file.display());
    Err(std::io::Error::new(gone.kind(), named))
}
