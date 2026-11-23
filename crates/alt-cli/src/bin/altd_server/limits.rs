//! What a request may bring: the bytes of its body and what the pack of
//! a push may inflate to.

/// hard cap on POST request bodies (push pack + upload-pack
/// command bodies). Bound by `ALT_SERVER_MAX_PUSH_BYTES`; default
/// 1 GiB matches what a healthy alt push against the dogfood corpus
/// fits inside, and stops a malicious or buggy client from streaming
/// gigabytes into memory through `std::io::copy(req.as_reader(), …)`.
pub(super) fn max_body_bytes() -> u64 {
    use std::sync::OnceLock;
    static CACHE: OnceLock<u64> = OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("ALT_SERVER_MAX_PUSH_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(1024 * 1024 * 1024)
    })
}

/// Read at most `max_body_bytes()` from `req.as_reader()` into `body`,
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
    req: &mut tiny_http::Request,
    body: &mut Vec<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;
    let max = max_body_bytes();
    let gzipped = req
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Encoding") && h.value.as_str().contains("gzip"));
    let limited = req.as_reader().take(max + 1);
    if gzipped {
        // Decompress under the same cap so a 100 KiB gzip bomb that
        // expands to 10 GiB still bounces.
        let mut decoder = alt_inflate::Reader::new(alt_inflate::Format::Gzip, limited);
        let mut decoded_cap = (&mut decoder).take(max + 1);
        decoded_cap.read_to_end(body)?;
    } else {
        let mut limited = limited;
        limited.read_to_end(body)?;
    }
    if body.len() as u64 > max {
        return Err(format!(
            "request body exceeds ALT_SERVER_MAX_PUSH_BYTES={max} (cap is per-request)"
        )
        .into());
    }
    Ok(())
}

/// How many times the bytes a push may bring its entries and objects may
/// inflate to, all together, unless `ALT_SERVER_MAX_INFLATED_BYTES` says.
const INFLATED_TIMES: u64 = 16;

/// What the pack of a push may hold. A pack is as its sender made it:
/// one small delta can stand for an object of any size, and each of a
/// million of them for another. No object may inflate to more than a
/// push may bring ([`max_body_bytes`]), which bounds the memory a push
/// takes; all of them together to no more than
/// `ALT_SERVER_MAX_INFLATED_BYTES`, sixteen times that by default, which
/// bounds the time.
pub(super) fn pack_limits() -> alt_git_pack::IndexLimits {
    use std::sync::OnceLock;
    static INFLATED: OnceLock<u64> = OnceLock::new();
    let object_bytes = max_body_bytes();
    let inflated_bytes = *INFLATED.get_or_init(|| {
        std::env::var("ALT_SERVER_MAX_INFLATED_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(object_bytes.saturating_mul(INFLATED_TIMES))
    });
    alt_git_pack::IndexLimits {
        object_bytes,
        inflated_bytes,
        kept: false,
        ..Default::default()
    }
}
