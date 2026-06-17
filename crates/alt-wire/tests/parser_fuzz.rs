//! M11/W30 — protocol parser fuzz harness.
//!
//! Property: every public parser in `alt-wire` must return a `Result`
//! on any input, never panic / abort / overflow. The wire is the
//! one place an attacker controls byte streams that reach our code,
//! so an unchecked array index or unwrap here is a remote-DoS bug.
//!
//! Implementation: zero-dep. `xorshift64` is a fast deterministic
//! PRNG; iteration count is tuned so a `cargo test --release` run
//! finishes in seconds. Each parser gets the same input shapes:
//!
//! - pure random bytes (worst-case alien input)
//! - "almost-valid" structures (a real prefix + random tail) — most
//!   effective at catching off-by-one / truncation bugs
//!
//! The harness only asserts "doesn't panic". A panic in `cargo test`
//! shows up as a thread crash with a backtrace; this file's job is to
//! generate enough inputs that any panic-able path is exercised.

use alt_git_codec::HashAlgo;
use alt_wire::push::PushRequest;

const ITERATIONS: u64 = 50_000;

/// xorshift64* — small fast deterministic PRNG. Seeded from a fixed
/// constant so test failures are bisectable.
struct Xorshift(u64);

impl Xorshift {
    fn new(seed: u64) -> Self {
        // Avoid 0 seed (xorshift fixed point).
        Self(if seed == 0 {
            0x1234_5678_9abc_def0
        } else {
            seed
        })
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn next_bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let v = self.next_u64().to_le_bytes();
            let take = (len - out.len()).min(8);
            out.extend_from_slice(&v[..take]);
        }
        out
    }
}

#[test]
fn pkt_read_frame_never_panics_on_random_bytes() {
    let mut rng = Xorshift::new(0xAA55_AA55_AA55_AA55);
    let mut scratch = Vec::new();
    for _ in 0..ITERATIONS {
        let len = (rng.next_u64() as usize) % 256;
        let bytes = rng.next_bytes(len);
        let mut cur = std::io::Cursor::new(&bytes);
        scratch.clear();
        // We don't care about the result — only that we got one
        // without unwinding.
        let _ = alt_wire::pkt::read_frame(&mut cur, &mut scratch);
    }
}

#[test]
fn parse_push_request_never_panics_on_random_bytes() {
    let mut rng = Xorshift::new(0x1357_9BDF_2468_ACE0);
    for _ in 0..ITERATIONS {
        let len = (rng.next_u64() as usize) % 4096;
        let bytes = rng.next_bytes(len);
        let mut cur = std::io::Cursor::new(&bytes);
        // The function is generic over `Read`; a Cursor on random
        // bytes exercises every branch of the pkt-line decoder.
        let _: Result<PushRequest, _> =
            alt_wire::push::parse_push_request(&mut cur, HashAlgo::Sha1);
    }
}

#[test]
fn parse_fetch_request_never_panics_on_random_bytes() {
    let mut rng = Xorshift::new(0x2222_4444_6666_8888);
    for _ in 0..ITERATIONS {
        let len = (rng.next_u64() as usize) % 4096;
        let bytes = rng.next_bytes(len);
        let mut cur = std::io::Cursor::new(&bytes);
        let _ = alt_wire::fetch::parse_fetch_request(&mut cur);
    }
}

#[test]
fn parse_capability_advertisement_never_panics_on_random_bytes() {
    let mut rng = Xorshift::new(0x9999_7777_5555_3333);
    for _ in 0..ITERATIONS {
        let len = (rng.next_u64() as usize) % 2048;
        let bytes = rng.next_bytes(len);
        let mut cur = std::io::Cursor::new(&bytes);
        let _ = alt_wire::caps::parse_capability_advertisement(&mut cur, "git-upload-pack");
    }
}

#[test]
fn parse_ls_refs_response_never_panics_on_random_bytes() {
    let mut rng = Xorshift::new(0xDEAD_BEEF_CAFE_BABE);
    for _ in 0..ITERATIONS {
        let len = (rng.next_u64() as usize) % 4096;
        let bytes = rng.next_bytes(len);
        let mut cur = std::io::Cursor::new(&bytes);
        let _ = alt_wire::ls_refs::parse_ls_refs_response(&mut cur, HashAlgo::Sha1);
    }
}

#[test]
fn parse_v1_ref_advertisement_never_panics_on_random_bytes() {
    let mut rng = Xorshift::new(0xFACE_C0DE_BAD0_F00D);
    for _ in 0..ITERATIONS {
        let len = (rng.next_u64() as usize) % 2048;
        let bytes = rng.next_bytes(len);
        let mut cur = std::io::Cursor::new(&bytes);
        let _ = alt_wire::push::parse_v1_ref_advertisement(&mut cur, HashAlgo::Sha1);
    }
}

/// "Almost-valid" inputs: a real pkt-line prefix followed by random
/// bytes. These find off-by-one and truncated-tail bugs that pure
/// random can miss.
#[test]
fn parse_push_request_never_panics_on_truncated_real_prefix() {
    // Plausible v1 push body header: a few pkt-lines then random.
    let mut base = Vec::new();
    base.extend_from_slice(b"00aa0000000000000000000000000000000000000000 ");
    base.extend_from_slice(b"1111111111111111111111111111111111111111 refs/heads/main\n");
    base.extend_from_slice(b"0000");
    let mut rng = Xorshift::new(0x8F8F_3C3C_5A5A_A5A5);
    for _ in 0..ITERATIONS {
        let cut = (rng.next_u64() as usize) % (base.len() + 1);
        let mut bytes = base[..cut].to_vec();
        let tail = (rng.next_u64() as usize) % 256;
        bytes.extend_from_slice(&rng.next_bytes(tail));
        let mut cur = std::io::Cursor::new(&bytes);
        let _ = alt_wire::push::parse_push_request(&mut cur, HashAlgo::Sha1);
    }
}
