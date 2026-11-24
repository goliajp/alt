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
pub(super) fn pack_limits() -> alt_git_pack::IndexLimits {
    static INDEXING: std::sync::OnceLock<alt_git_pack::Memory> = std::sync::OnceLock::new();
    let told = super::told::told();
    alt_git_pack::IndexLimits {
        object_bytes: told.push_bytes,
        inflated_bytes: told.inflated_bytes,
        kept: false,
        memory: Some(INDEXING.get_or_init(|| alt_git_pack::Memory::new(told.index_bytes))),
        memory_by: Some(std::time::Instant::now() + told.index_wait),
        files: told.bases.as_ref(),
        ..Default::default()
    }
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
