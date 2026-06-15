//! git smart-http protocol v2: the byte-level encoding/decoding stone for
//! the wire. Pure logic — bytes in, structured frames out; structured
//! commands in, bytes out. Transport (HTTPS) is in a sibling crate.
//!
//! ## Scope (W1 + W4)
//!
//! - **pkt-line** framing: every git protocol byte stream is a sequence of
//!   `pkt::Frame`s — `Data(bytes)`, `Flush`, `Delim`, `ResponseEnd` — with a
//!   four-byte hex length prefix.
//! - **Capability advertisement** (server → client `GET …/info/refs?service=…`
//!   response): protocol version + per-command capability map.
//! - **ls-refs** command: structured request encoding + response parsing
//!   (a list of `RefRecord { name, oid, peeled, symref_target }`).
//! - **fetch** command (W4): request encoding (wants / haves / done / flags)
//!   plus a section-aware preamble parser and a sideband demuxer for the
//!   packfile stream — pack bytes tee out to a caller-supplied indexer.
//!
//! Push request bodies live in W5; this crate still has no I/O — the HTTP
//! transport (W2) is a transparent byte mover.
//!
//! ## Why hand-written
//!
//! `gix` and `libgit2` both speak this protocol, but pulling either in
//! would more than triple alt's dependency surface (gix is ~50+ crates;
//! libgit2 is a C library). Protocol v2 is one document
//! (`git/Documentation/gitprotocol-v2.txt`) plus pkt-line, and `alt-git-pack`
//! already does the heavy lifting for pack streams. Keeping wire in-house
//! preserves the project's minimal-dep / zero-C stance.
//!
//! ## Stone
//!
//! No I/O, no business types. Frames in / frames out. Errors are typed so
//! transport callers can react (e.g. "this stream truncated mid-pkt" is a
//! different signal from "the server sent us an unparseable ref line").
//! Fuzz invariants: any byte sequence decodes panic-free; the encoded form
//! of any structured request round-trips through decode.

pub mod caps;
pub mod fetch;
pub mod ls_refs;
pub mod pkt;

pub use caps::{CapabilityAd, parse_capability_advertisement};
pub use fetch::{
    FetchAck, FetchError, FetchPreamble, FetchRequest, ShallowInfo, WantedRef, drain_packfile,
    encode_fetch_request, read_fetch_preamble,
};
pub use ls_refs::{LsRefsRequest, RefRecord, encode_ls_refs_request, parse_ls_refs_response};
pub use pkt::{
    Frame, PktError, read_frame, write_data, write_delim, write_flush, write_response_end,
};
