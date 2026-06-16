//! M6/W9 — the alt-to-alt wire signing extension.
//!
//! Verifies that when local signing is on, `alt push` declares the
//! `alt-principal` + `alt-sig` capabilities in its receive-pack request
//! and that the signature verifies against the principal's pubkey over
//! the canonical push payload.
//!
//! A real git server silently ignores unknown caps, so the wire side is
//! safe; here we use a body-capture listener instead of a full git
//! receive-pack proxy so the test can inspect the exact bytes alt put
//! on the wire.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "alice")
        .env("GIT_AUTHOR_EMAIL", "a@e")
        .env("ALT_PRINCIPAL_ID", "alice")
        .env("USER", "alice")
        .args(args)
        .output()
        .unwrap()
}

fn ok(label: &str, o: Output) -> Output {
    assert!(
        o.status.success(),
        "{label} failed: stderr={} stdout={}",
        String::from_utf8_lossy(&o.stderr),
        String::from_utf8_lossy(&o.stdout),
    );
    o
}

/// Spawn a tiny HTTP listener that ALWAYS reports a clean push (so the
/// CLI side-effects fire) while capturing the request body for the test
/// to assert against. The first GET `info/refs` returns an
/// empty-repo-style v1 advertisement; the POST body is recorded.
fn spawn_capture_server() -> (String, Arc<Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}");
    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
    let captured_t = Arc::clone(&captured);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let captured = Arc::clone(&captured_t);
            thread::spawn(move || {
                let _ = handle_one(stream, captured);
            });
        }
    });
    (url, captured)
}

fn handle_one(
    mut stream: std::net::TcpStream,
    captured: Arc<Mutex<Vec<u8>>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let parts: Vec<&str> = line.trim_end().split(' ').collect();
    if parts.len() < 3 {
        return Ok(());
    }
    let method = parts[0].to_owned();
    let url = parts[1].to_owned();
    let mut content_length: usize = 0;
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        let t = h.trim_end_matches(['\r', '\n']);
        if t.is_empty() {
            break;
        }
        if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    let (status, payload, ct) = if method == "GET" && url.starts_with("/info/refs") {
        // empty-repo v1 ref ad: smart-http envelope + capabilities^{} row
        // with caps the alt client expects (report-status, side-band-64k)
        let mut body_out = Vec::new();
        write_pkt(&mut body_out, b"# service=git-receive-pack\n");
        body_out.extend_from_slice(b"0000");
        let zero = "0".repeat(40);
        let mut cap_line = format!("{zero} capabilities^{{}}").into_bytes();
        cap_line.push(0);
        cap_line
            .extend_from_slice(b"report-status side-band-64k delete-refs ofs-delta agent=git/test");
        cap_line.push(b'\n');
        write_pkt(&mut body_out, &cap_line);
        body_out.extend_from_slice(b"0000");
        (
            "200 OK",
            body_out,
            "application/x-git-receive-pack-advertisement",
        )
    } else if method == "POST" && url == "/git-receive-pack" {
        // record the body so the test can dissect the cap list
        *captured.lock().unwrap() = body.clone();
        // minimal sideband-wrapped report-status: "unpack ok\n",
        // "ok refs/heads/main\n", flush — wrapped in band 1
        let mut inner = Vec::new();
        write_pkt(&mut inner, b"unpack ok\n");
        write_pkt(&mut inner, b"ok refs/heads/main\n");
        inner.extend_from_slice(b"0000");
        let mut band1 = vec![1u8];
        band1.extend_from_slice(&inner);
        let mut outer = Vec::new();
        write_pkt(&mut outer, &band1);
        outer.extend_from_slice(b"0000");
        ("200 OK", outer, "application/x-git-receive-pack-result")
    } else {
        ("404 Not Found", Vec::new(), "text/plain")
    };

    let resp_head = format!(
        "HTTP/1.0 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(resp_head.as_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()?;
    Ok(())
}

fn write_pkt(out: &mut Vec<u8>, payload: &[u8]) {
    let total = payload.len() + 4;
    out.extend_from_slice(format!("{total:04x}").as_bytes());
    out.extend_from_slice(payload);
}

/// With `<alt-dir>/sign-policy` enabled, alt push appends
/// `alt-principal=<id>` and `alt-sig=alt-sig-ed25519:<sig>` to the
/// capability list; the signature is over the canonical push payload
/// (sorted `<old> <new> <name>\n` lines) and verifies against the
/// principal's public key.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn alt_push_attaches_alt_principal_and_sig_caps_when_signing_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    ok("alt init", alt(root, &["init", "."]));
    ok("identity init", alt(root, &["identity", "init", "alice"]));
    std::fs::write(
        root.join(".alt/sign-policy"),
        "enabled = true\nprincipal = alice\n",
    )
    .unwrap();
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    ok("alt add", alt(root, &["add", "."]));
    ok("alt commit", alt(root, &["commit", "-m", "first"]));

    let (url, captured) = spawn_capture_server();
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );

    ok("alt push", alt(root, &["push", "origin"]));
    let body = captured.lock().unwrap().clone();
    assert!(!body.is_empty(), "capture server should have received POST");

    // the body starts with `<4-hex length><payload>` (pkt-line framing);
    // skip the prefix, then the payload is "<old> <new> <name>\0<caps>\n"
    let len_hex = std::str::from_utf8(&body[..4]).expect("body should be pkt-line ascii");
    let first_pkt_len = usize::from_str_radix(len_hex, 16).expect("hex length");
    let first_payload = &body[4..first_pkt_len];
    let nul = first_payload
        .iter()
        .position(|&b| b == 0)
        .expect("NUL not found in first pkt payload");
    let cap_end = first_payload[nul + 1..]
        .iter()
        .position(|&b| b == b'\n')
        .expect("LF terminator not found in cap line");
    let caps = std::str::from_utf8(&first_payload[nul + 1..nul + 1 + cap_end]).unwrap();
    assert!(caps.contains("report-status"), "{caps}");
    assert!(
        caps.contains("alt-principal=alice"),
        "alt-principal cap missing: {caps}"
    );
    let sig_cap = caps
        .split_whitespace()
        .find(|c| c.starts_with("alt-sig="))
        .unwrap_or_else(|| panic!("alt-sig cap missing: {caps}"));
    let sig_text = sig_cap.strip_prefix("alt-sig=").unwrap();
    let sig = alt_sign::Sig::from_text(sig_text).expect("sig parses");

    // verify against alice's pubkey over the canonical payload
    let pub_text = std::fs::read_to_string(root.join(".alt/identity/alice.pub")).unwrap();
    let pubkey = alt_sign::PublicKey::from_text(&pub_text).unwrap();

    // reconstruct the updates list from the body so we can re-derive the
    // exact bytes that should have been signed
    let head_line = std::str::from_utf8(&first_payload[..nul]).unwrap();
    let mut parts = head_line.splitn(3, ' ');
    let old_hex = parts.next().unwrap();
    let new_hex = parts.next().unwrap();
    let name = parts.next().unwrap();
    let zero = "0".repeat(40);
    let payload = format!("{old_hex} {new_hex} {name}\n");
    assert_eq!(old_hex, zero, "first push has zero old oid");
    pubkey
        .verify(payload.as_bytes(), &sig)
        .expect("sig should verify over the canonical payload");
}

/// Without signing policy, the push request never carries the alt-*
/// caps. Confirms the extension is fully opt-in.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn alt_push_does_not_attach_caps_when_signing_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok("alt init", alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    ok("alt add", alt(root, &["add", "."]));
    ok("alt commit", alt(root, &["commit", "-m", "first"]));

    let (url, captured) = spawn_capture_server();
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );
    ok("alt push", alt(root, &["push", "origin"]));

    let body = captured.lock().unwrap().clone();
    assert!(!body.is_empty(), "capture server should have received POST");
    assert!(
        !body
            .windows(b"alt-principal=".len())
            .any(|w| w == b"alt-principal="),
        "alt-principal should be absent when signing is off"
    );
    assert!(
        !body.windows(b"alt-sig=".len()).any(|w| w == b"alt-sig="),
        "alt-sig should be absent when signing is off"
    );
}
