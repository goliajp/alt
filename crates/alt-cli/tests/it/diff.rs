//! `alt diff`: unstaged (index → work tree) and `--cached` (HEAD → index),
//! with binary detection. The hunk body is cross-checked against real git.

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

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

/// The unified body (from the first `@@` on) of a diff, so we can compare our
/// output to git's without coupling to the exact header bytes.
fn hunk_body(diff: &str) -> String {
    match diff.find("@@") {
        Some(i) => diff[i..].to_string(),
        None => String::new(),
    }
}

#[test]
fn diff_unstaged_and_cached_match_git_hunks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f.txt"), "line1\nline2\nline3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));

    // a clean tree has no diff
    assert!(ok(alt(root, &["diff"])).is_empty(), "clean tree => no diff");

    // edit the file in the working tree; `alt diff` shows the unstaged change
    std::fs::write(root.join("f.txt"), "line1\nCHANGED\nline3\nline4\n").unwrap();
    let unstaged = ok(alt(root, &["diff"]));
    assert!(
        unstaged.contains("diff --git a/f.txt b/f.txt"),
        "{unstaged}"
    );
    assert!(unstaged.contains("--- a/f.txt"), "{unstaged}");

    // cross-check the hunk body against real git on the same edit
    let gdir = tempfile::tempdir().unwrap();
    let groot = gdir.path();
    ok(git(groot, &["init", "-q", "."]));
    ok(git(groot, &["config", "user.email", "t@e"]));
    ok(git(groot, &["config", "user.name", "t"]));
    std::fs::write(groot.join("f.txt"), "line1\nline2\nline3\n").unwrap();
    ok(git(groot, &["add", "."]));
    ok(git(groot, &["commit", "-qm", "first"]));
    std::fs::write(groot.join("f.txt"), "line1\nCHANGED\nline3\nline4\n").unwrap();
    let gdiff = ok(git(groot, &["-c", "core.pager=cat", "diff"]));
    assert_eq!(
        hunk_body(&unstaged),
        hunk_body(&gdiff),
        "hunk body must match git"
    );

    // before staging, --cached is empty; after add it shows the staged change
    assert!(
        ok(alt(root, &["diff", "--cached"])).is_empty(),
        "nothing staged yet"
    );
    ok(alt(root, &["add", "."]));
    let cached = ok(alt(root, &["diff", "--cached"]));
    assert_eq!(
        hunk_body(&cached),
        hunk_body(&gdiff),
        "staged hunk matches git"
    );
    // and now the working tree matches the index, so unstaged is empty
    assert!(ok(alt(root, &["diff"])).is_empty(), "work tree == index");

    // a new staged file is a full addition
    std::fs::write(root.join("new.txt"), "alpha\nbeta\n").unwrap();
    ok(alt(root, &["add", "."]));
    let added = ok(alt(root, &["diff", "--cached"]));
    assert!(added.contains("new file mode 100644"), "{added}");
    assert!(added.contains("--- /dev/null"), "{added}");
    assert!(added.contains("+alpha\n+beta\n"), "{added}");

    // binary content is reported, not dumped
    std::fs::write(root.join("b.bin"), b"\x00\x01\x02bin\x00").unwrap();
    ok(alt(root, &["add", "."]));
    let bin = ok(alt(root, &["diff", "--cached"]));
    assert!(
        bin.contains("Binary files a/b.bin and b/b.bin differ"),
        "{bin}"
    );
    // E2: human view also gets an A8 B1 chunk-diff summary line — counts and
    // a percentage so the reader knows whether this is a tiny change or a
    // full rewrite without opening the bytes.
    assert!(
        bin.contains("chunks: ") && bin.contains("bytes shared)"),
        "missing chunk-diff summary line: {bin}"
    );
}

/// E3b (A8b): `alt diff --semantic` for a `.rs` file replaces the unified
/// hunks with an item-level AST summary — a single logical change keyed on
/// the function whose body moved, other items silent.
#[test]
fn semantic_diff_for_rust_shows_one_logical_change_per_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("lib.rs"), "fn keep() {}\nfn touch() { 1 }\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    std::fs::write(root.join("lib.rs"), "fn keep() {}\nfn touch() { 2 }\n").unwrap();

    let out = ok(alt(root, &["diff", "--semantic"]));
    assert!(out.contains("diff --git a/lib.rs b/lib.rs"), "{out}");
    assert!(out.contains("logical changes:"), "{out}");
    assert!(out.contains("  fn:touch"), "{out}");
    assert!(!out.contains("  fn:keep"), "keep should be silent: {out}");
    // unified-diff hunks should NOT appear under --semantic (the summary
    // replaces them)
    assert!(
        !out.contains("@@"),
        "no unified hunks under --semantic: {out}"
    );
}

/// `--semantic` on a non-Rust path falls through to the regular line diff,
/// so a mixed-language commit still shows everything (semantic resolution
/// is a refinement, not a contract — signal is never lost).
#[test]
fn semantic_diff_falls_back_to_line_diff_for_unsupported_languages() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("notes.txt"), "v1\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    std::fs::write(root.join("notes.txt"), "v2\n").unwrap();

    let out = ok(alt(root, &["diff", "--semantic"]));
    assert!(out.contains("@@"), "txt fallback to line diff: {out}");
    assert!(out.contains("-v1"), "{out}");
    assert!(out.contains("+v2"), "{out}");
    assert!(!out.contains("logical changes:"), "no AST surface: {out}");
}

/// Build a minimal valid 8-bit grayscale PNG (single IDAT, filter byte 0
/// per scanline). Good enough for the M7-B3 perceptual-diff path: it walks
/// PNG chunks, takes the IDAT bytes, inflates and fingerprints — no CRC
/// check, no IHDR parsing required.
fn build_minimal_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;
    assert_eq!(pixels.len() as u32, width * height);

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0; 4]); // placeholder CRC (alt's reader ignores it)
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // depth 8 / greyscale / std compression / no filter / no interlace
    chunk(&mut out, b"IHDR", &ihdr);

    let mut raw = Vec::with_capacity((width * height + height) as usize);
    for row in 0..height as usize {
        raw.push(0); // PNG filter type None
        let start = row * width as usize;
        raw.extend_from_slice(&pixels[start..start + width as usize]);
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(&raw).unwrap();
    chunk(&mut out, b"IDAT", &encoder.finish().unwrap());
    chunk(&mut out, b"IEND", &[]);
    out
}

/// M7-B3: `alt diff` on a PNG that was changed should land a perceptual
/// diff hint alongside the chunk-diff summary — both in the human view
/// ("perceptual diff: N% off (prism=png)") and the JSON
/// (`perceptual_diff: {kind, prism, distance}`). A small change must
/// produce a non-trivial distance (the bytes-shared chunk ratio alone
/// would mislead a reader who hadn't seen the perceptual line: a tiny
/// pixel-block tweak rewrites the entire zlib stream).
#[test]
fn diff_png_change_reports_perceptual_hint() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));

    // 16x16 grayscale; the second image flips the top-left 8x8 quadrant
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
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "initial png"]));

    std::fs::write(root.join("img.png"), build_minimal_png(16, 16, &tweaked)).unwrap();
    ok(alt(root, &["add", "."]));

    let text = ok(alt(root, &["diff", "--cached"]));
    assert!(
        text.contains("Binary files a/img.png and b/img.png differ"),
        "git-compat binary line missing: {text}"
    );
    assert!(
        text.contains("chunks: "),
        "chunk-diff summary missing: {text}"
    );
    assert!(
        text.contains("perceptual diff:") && text.contains("(prism=png)"),
        "perceptual hint missing: {text}"
    );
    // M10/W20 (B2): the part-aware line surfaces *which* PNG chunk
    // changed; IHDR is byte-identical (same dimensions / colour type
    // on both sides) so it must NOT appear in the line, while IDAT
    // (the pixel stream) must.
    assert!(
        text.contains("png: ") && text.contains("IDAT"),
        "part-aware PNG line missing: {text}"
    );
    assert!(
        !text.contains("IHDR"),
        "IHDR is unchanged here and must be dropped from the line: {text}"
    );
    // the second image differs from the first; a 0% off reading would
    // mean the hash collapsed to identical and the metric carries no
    // signal — that's a regression even if the line is present.
    assert!(
        !text.contains("perceptual diff: 0% off"),
        "perceptual distance should be non-zero on a real pixel change: {text}"
    );

    let json = ok(alt(root, &["diff", "--cached", "--json"]));
    assert!(
        json.contains("\"perceptual_diff\":{\"kind\":\"perceptual_diff\""),
        "perceptual_diff json object missing: {json}"
    );
    assert!(
        json.contains("\"prism\":\"png\""),
        "prism=png missing in json: {json}"
    );
    assert!(
        json.contains("\"distance\":") && !json.contains("\"distance\":0.0"),
        "distance should be present and non-zero: {json}"
    );
    // M10/W20 (B2): structured part-aware breakdown rides under
    // `part_diff`. all_same=false because IDAT changed.
    assert!(
        json.contains("\"part_diff\":{\"kind\":\"part_diff\""),
        "part_diff json object missing: {json}"
    );
    assert!(
        json.contains("\"all_same\":false"),
        "part_diff must report all_same=false: {json}"
    );
    assert!(
        json.contains("\"name\":\"IDAT\",\"status\":\"changed\""),
        "IDAT must be reported as changed: {json}"
    );
    assert!(
        json.contains("\"name\":\"IHDR\",\"status\":\"same\""),
        "IHDR is byte-equal and must be reported as same: {json}"
    );
}

/// Non-image binary content keeps the existing chunk-diff summary but
/// the perceptual hint stays silent (text) / null (json) — additive, no
/// false signal on generic binary blobs.
#[test]
fn diff_generic_binary_omits_perceptual_hint() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.bin"), b"\x00\x01\x02first\x00").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    std::fs::write(root.join("a.bin"), b"\x00\x01\x02second\x00").unwrap();
    ok(alt(root, &["add", "."]));

    let text = ok(alt(root, &["diff", "--cached"]));
    assert!(text.contains("chunks: "), "chunk summary expected: {text}");
    assert!(
        !text.contains("perceptual diff:"),
        "no perceptual hint on generic binary: {text}"
    );
    let json = ok(alt(root, &["diff", "--cached", "--json"]));
    assert!(
        json.contains("\"perceptual_diff\":null"),
        "perceptual_diff must be null for generic binary: {json}"
    );
}

/// E3b JSON: each file entry gains an `ast_diff` field under `--semantic`
/// for languages with a parser; un-`--semantic` runs leave it null even
/// for `.rs` files (the field is additive, not always-on).
#[test]
fn semantic_diff_json_carries_ast_diff_field() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.rs"), "fn keep() {}\nfn touch() { 1 }\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    std::fs::write(root.join("a.rs"), "fn keep() {}\nfn touch() { 2 }\n").unwrap();

    // without --semantic: ast_diff is null even for a Rust file
    let plain = ok(alt(root, &["diff", "--json"]));
    assert!(
        plain.contains("\"ast_diff\":null"),
        "without --semantic, ast_diff stays null: {plain}"
    );

    // with --semantic: kind=ast_diff, fn:touch is logical, no false positives
    let json = ok(alt(root, &["diff", "--json", "--semantic"]));
    assert!(
        json.contains("\"ast_diff\":{\"kind\":\"ast_diff\""),
        "ast_diff payload missing: {json}"
    );
    assert!(
        json.contains("\"logical_changes\":[\"fn:touch\"]"),
        "logical_changes mismatch: {json}"
    );
    assert!(
        json.contains("\"is_format_only\":false"),
        "is_format_only mismatch: {json}"
    );
}
