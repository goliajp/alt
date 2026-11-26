//! What a request may bring: the bytes of its body and what the pack of
//! a push may inflate to.

/// hard cap on POST request bodies (push pack + upload-pack
/// command bodies). Bound by `ALT_SERVER_MAX_PUSH_BYTES`; default
/// 1 GiB matches what a healthy alt push against the dogfood corpus
/// fits inside, and stops a malicious or buggy client from streaming
/// gigabytes into memory through `std::io::copy(req.as_reader(), …)`.
pub(super) fn max_body_bytes() -> u64 {
    super::told::told().push_bytes
}

/// Read at most `max` bytes from `req.as_reader()` into `body`,
/// transparently gunzip'ing when the client set `Content-Encoding:
/// gzip`. Returns `Err` when the client streamed past the cap — the
/// handler maps that into a 413, never an OOM. We don't trust
/// `body_length()` alone (a malicious client can send a small
/// Content-Length header and then keep writing); the `.take()` is
/// the real enforcement, and it bounds BOTH the on-wire (compressed)
/// bytes and the decoded output.
///
/// Background: git's smart-http client transparently sets
/// `Content-Encoding: gzip` on POST bodies it expects to compress
/// well (push pack payloads, repeated want/have lists). Without
/// gunzip on the server, the alt-wire pkt-line parser sees the gzip
/// magic `1f 8b 08 …` and bails with "length prefix is not valid
/// hex" — which the stress harness deterministically hit at scale.
pub(super) fn read_body_capped(
    req: &mut super::http::Request,
    body: &mut Vec<u8>,
    max: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;
    let gzipped = req
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Encoding") && h.value.as_str().contains("gzip"));
    let over = max.saturating_add(1);
    let limited = req.body(over).take(over);
    if gzipped {
        // Decompress under the same cap so a 100 KiB gzip bomb that
        // expands to 10 GiB still bounces.
        let mut decoder = alt_inflate::Reader::new(alt_inflate::Format::Gzip, limited);
        let mut decoded_cap = (&mut decoder).take(over);
        decoded_cap.read_to_end(body)?;
    } else {
        let mut limited = limited;
        limited.read_to_end(body)?;
    }
    if body.len() as u64 > max {
        return Err(format!("request body exceeds {max} bytes (cap is per-request)").into());
    }
    Ok(())
}

/// The most bytes of a request that are held in memory: the commands of
/// a fetch, and those of a push before its pack. A want or a ref update
/// is some hundred bytes, so this is room for some hundred thousand of
/// them, and what a request that is all commands can make the server
/// hold.
const COMMANDS: u64 = 64 << 20;

pub(super) fn max_command_bytes() -> u64 {
    max_body_bytes().min(COMMANDS)
}

/// The bytes on the wire the body of a request to the endpoint `suffix`
/// ends in may take: one more than it may have, so that one over is told
/// by its count.
pub(super) fn body_cap(suffix: &str) -> u64 {
    let most = match suffix.ends_with("/git-receive-pack") {
        true => max_body_bytes(),
        false => max_command_bytes(),
    };
    most.saturating_add(1)
}

/// What the pack of a push may hold. A pack is as its sender made it:
/// one small delta can stand for an object of any size, and each of a
/// million of them for another. No object may inflate to more than a
/// push may bring ([`max_body_bytes`]), which bounds the memory a push
/// takes; all of them together to no more than
/// `ALT_SERVER_MAX_INFLATED_BYTES`, 128 times that by default,
/// which bounds the time.
///
/// The memory of the packs indexed at once is one amount, and a pack
/// waits for its part of it for `ALT_SERVER_MAX_INDEX_WAIT_MS`.
///
/// A push whose peer is gone waits for nothing and is indexed no further.
pub(super) fn pack_limits(of: &super::http::Request) -> alt_git_pack::IndexLimits<'_> {
    static INDEXING: std::sync::OnceLock<alt_git_pack::Memory> = std::sync::OnceLock::new();
    let told = super::told::told();
    alt_git_pack::IndexLimits {
        object_bytes: told.push_bytes,
        inflated_bytes: told.inflated_bytes,
        kept: false,
        // a server told to index in little memory still has some for each pack
        each_bytes: alt_git_pack::EACH.min(told.index_bytes / 16),
        memory: Some(INDEXING.get_or_init(|| alt_git_pack::Memory::new(told.index_bytes))),
        memory_by: Some(std::time::Instant::now() + told.index_wait),
        given_up: Some(of.given_up()),
        files: told.bases.as_ref(),
        ..Default::default()
    }
}

/// The answer to a push that waited for the memory other pushes are indexed
/// in for as long as one waits.
pub(super) fn busy() -> super::http::Response<std::io::Cursor<Vec<u8>>> {
    let busy = "the packs of other pushes are being indexed; send the push again";
    let mut response = super::http::Response::from_string(busy).with_status_code(503);
    response.add_header(super::upload::header("Retry-After", "60"));
    response
}

/// The status a push is refused with where its commands could not be read:
/// that of what kept its body from coming where something did, and
/// otherwise that of a request that is none.
pub(super) fn unread_commands(error: &(dyn std::error::Error + 'static)) -> u16 {
    use std::io::ErrorKind;
    let mut cause = Some(error);
    while let Some(error) = cause {
        match error.downcast_ref::<std::io::Error>().map(|io| io.kind()) {
            Some(ErrorKind::TimedOut) => return 408,
            Some(ErrorKind::StorageFull) => return 503,
            Some(ErrorKind::FileTooLarge) => return 413,
            _ => cause = error.source(),
        }
    }
    400
}

/// The status a request is refused with where its body could not be read
/// within its cap: the peer stopped sending, ended early, sent what is
/// no body, or sent too much, or the server has no room for it now.
pub(super) fn refusal(error: &(dyn std::error::Error + 'static)) -> u16 {
    use std::io::ErrorKind;
    match error
        .downcast_ref::<std::io::Error>()
        .map(std::io::Error::kind)
    {
        Some(ErrorKind::TimedOut) => 408,
        Some(ErrorKind::UnexpectedEof | ErrorKind::InvalidData) => 400,
        Some(ErrorKind::StorageFull) => 503,
        _ => 413,
    }
}

/// What the peer of a request is told of `error`, an error of the server in
/// doing `what`: the error where the peer signed in, and otherwise `what`
/// and the kind of the error of the system it comes of, if any. Those name
/// no file of the server: the error itself goes to its log.
pub(super) fn told_of(
    error: &(dyn std::error::Error + 'static),
    what: &str,
    signed_in: bool,
) -> String {
    if signed_in {
        return error.to_string();
    }
    let mut cause = Some(error);
    while let Some(error) = cause {
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            return format!("{what}: {}", io.kind());
        }
        cause = error.source();
    }
    what.to_owned()
}

/// [`told_of`] for a line that names what failed in either case: `what`
/// and, where the peer signed in, the error with what caused it.
pub(super) fn told_at(
    error: &(dyn std::error::Error + 'static),
    what: &str,
    signed_in: bool,
) -> String {
    match signed_in {
        true => format!("{what}: {}", alt_cli::machine::causes(error)),
        false => told_of(error, what, false),
    }
}

/// What a push is told of an error of the server at a step, as
/// [`told_at`] has it for a peer `signed_in` or not; the error itself goes
/// to the log whatever the peer is told.
pub(super) fn telling(
    signed_in: bool,
) -> impl Fn(&str, &(dyn std::error::Error + 'static)) -> String {
    move |what, error| {
        let causes = alt_cli::machine::causes(error);
        eprintln!("altd-server: push: {what}: {causes}");
        told_at(error, what, signed_in)
    }
}

/// The status of the answer to a fetch whose response was not made for
/// `error`, what it tells the peer, and whether `error` is one of the
/// server. Of an error of the server, the want of room on its disk among
/// them, a peer that is not `signed_in` is told only the kind; a fetch whose
/// time is over, or whose response has no room beside those being sent, is
/// told so as it is.
pub(super) fn fetch_refused(
    error: &(dyn std::error::Error + 'static),
    late: bool,
    signed_in: bool,
) -> (u16, String, bool) {
    let io = error.downcast_ref::<std::io::Error>();
    let full = io.and_then(|io| io.get_ref()?.downcast_ref::<super::sending::Full>());
    if late || full.is_some() {
        return (503, error.to_string(), false);
    }
    match refusal(error) {
        503 => {
            let what = "the server had no room to make the pack";
            (503, told_of(error, what, signed_in), true)
        }
        _ => {
            let what = "the server failed to make the pack";
            (500, told_of(error, what, signed_in), true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fetch the server had no room for is told so; a peer that is not
    /// signed in is not told the file the room was wanted for
    #[test]
    fn a_fetch_there_is_no_room_for_is_told_the_file_where_signed_in_alone() {
        let full = || {
            let why = "No space left on device (os error 28) at path \"/srv/tmp/.tmpAbc\"";
            std::io::Error::new(std::io::ErrorKind::StorageFull, why)
        };
        let (status, told, erred) = fetch_refused(&full(), false, false);
        assert_eq!(
            (status, told.as_str(), erred),
            (
                503,
                "the server had no room to make the pack: no storage space",
                true
            )
        );
        let (status, told, _) = fetch_refused(&full(), false, true);
        assert_eq!(status, 503);
        assert!(told.contains("/srv/tmp/.tmpAbc"), "{told}");
    }

    /// A fetch whose response has no room beside those being sent is told
    /// so, signed in or not
    #[test]
    fn a_fetch_whose_response_has_no_room_beside_others_is_told_so() {
        let full =
            std::io::Error::new(std::io::ErrorKind::StorageFull, super::super::sending::Full);
        let (status, told, erred) = fetch_refused(&full, false, false);
        let full = super::super::sending::FULL;
        assert_eq!((status, told.as_str(), erred), (503, full, false));
    }

    /// A fetch whose time is over is told so, signed in or not
    #[test]
    fn a_fetch_whose_time_is_over_is_told_so() {
        let late = super::super::until::Stopped::Late;
        let (status, told, erred) = fetch_refused(&late, true, false);
        assert_eq!((status, told, erred), (503, late.to_string(), false));
    }
}
