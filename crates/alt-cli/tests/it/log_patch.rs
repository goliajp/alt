//! M7-B4: `alt log -p` emits per-commit patches. Text files get a unified
//! diff (cross-checked against git); binary files stay compact — a
//! chunk-diff summary + a perceptual hint when the content is a kind we
//! recognise — so a binary-asset history doesn't blow up the terminal.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn build_minimal_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    use flate2::{Compression, write::ZlibEncoder};

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0; 4]); // placeholder CRC (alt's reader ignores it)
    }

    assert_eq!(pixels.len() as u32, width * height);
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);

    let mut raw = Vec::with_capacity((width * height + height) as usize);
    for row in 0..height as usize {
        raw.push(0);
        let start = row * width as usize;
        raw.extend_from_slice(&pixels[start..start + width as usize]);
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(&raw).unwrap();
    chunk(&mut out, b"IDAT", &encoder.finish().unwrap());
    chunk(&mut out, b"IEND", &[]);
    out
}

#[test]
fn log_patch_emits_text_unified_diff() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    std::fs::write(root.join("a.txt"), "one\nTWO\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "second"]));

    let log = ok(alt(root, &["log", "-p", "--pretty=oneline"]));
    assert!(log.contains("diff --git a/a.txt b/a.txt"), "{log}");
    assert!(log.contains("@@"), "unified hunk expected: {log}");
    assert!(log.contains("-two") && log.contains("+TWO"), "{log}");
    // first commit was a fresh add → mode line + /dev/null - side
    assert!(
        log.contains("new file mode") && log.contains("--- /dev/null"),
        "first commit must surface as an add: {log}"
    );
}

#[test]
fn log_patch_compacts_binary_blobs_with_chunk_and_perceptual_lines() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));

    // baseline grayscale + a quadrant-flipped variant: triggers the
    // perceptual fingerprint (PNG) and keeps the byte payload tiny so the
    // unified-diff path would otherwise dump the bytes
    let mut base = vec![0u8; 16 * 16];
    for y in 0..16 {
        for x in 0..16 {
            base[y * 16 + x] = if (x + y) & 1 == 0 { 0 } else { 255 };
        }
    }
    let mut tweaked = base.clone();
    for y in 0..8 {
        for x in 0..8 {
            tweaked[y * 16 + x] = 255 - tweaked[y * 16 + x];
        }
    }
    std::fs::write(root.join("img.png"), build_minimal_png(16, 16, &base)).unwrap();
    std::fs::write(root.join("plain.bin"), b"\x00\x01\x02first\x00").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));

    std::fs::write(root.join("img.png"), build_minimal_png(16, 16, &tweaked)).unwrap();
    std::fs::write(root.join("plain.bin"), b"\x00\x01\x02second\x00").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "second"]));

    let log = ok(alt(root, &["log", "-p", "--pretty=oneline"]));
    assert!(
        log.contains("Binary files a/img.png and b/img.png differ"),
        "{log}"
    );
    assert!(
        log.contains("chunks: "),
        "chunk-diff summary expected: {log}"
    );
    assert!(
        log.contains("perceptual diff:") && log.contains("(prism=png)"),
        "perceptual hint expected on PNG change: {log}"
    );
    // generic binary keeps the compact summary but stays silent on
    // perceptual (no false signal)
    assert!(
        log.contains("Binary files a/plain.bin and b/plain.bin differ"),
        "{log}"
    );

    // crucial regression: log -p must never dump the raw PNG bytes. The
    // PNG envelope starts with the standard signature; if that magic
    // ever leaks into stdout the binary path collapsed back to a unified
    // diff and large-file history would be unusable.
    let png_sig = [0x89_u8, b'P', b'N', b'G'];
    assert!(
        !log.as_bytes().windows(4).any(|w| w == png_sig),
        "log -p must not dump raw binary bytes"
    );
}

#[test]
fn log_patch_first_commit_shows_all_files_as_additions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("hello.txt"), "hello\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "initial"]));

    let log = ok(alt(root, &["log", "-p", "--pretty=oneline"]));
    assert!(log.contains("diff --git a/hello.txt b/hello.txt"), "{log}");
    assert!(log.contains("new file mode 100644"), "{log}");
    assert!(log.contains("+hello"), "{log}");
}
