//! M7-B1 large-files corpus generator.
//!
//! Builds a deterministic ~80 MB git repo of mixed binary assets at
//! `<dir>`: a few "image"-shaped files (real libz-deflated streams,
//! enough zlib for the prism layer to fire), several pseudo-random data
//! files (incompressible — the CDC stress case), one archive-shaped file
//! that concatenates multiple deflate streams (real-world container
//! shape), plus a tiny manifest. Five commits then exercise the modify
//! patterns that B2 / B3 need to benchmark:
//!
//!   C1: initial drop
//!   C2: in-place pixel tweak on image01 (changes its zlib stream)
//!   C3: append to dataset01 (lineage delta on a large blob)
//!   C4: mid-file mod to archive (chunk-level dedup story)
//!   C5: full replace of image04 (whole-blob churn)
//!
//! Reproducible — same seed → same bytes — so a corpus rebuild diff is a
//! tooling regression, not a fixture drift. Invoked via
//! `cargo run -p alt-testutil --bin build-large-corpus -- <dir>` or the
//! `scripts/build-large-corpus.sh` wrapper that defaults to
//! `.claude/corpus/large-files`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::Compression;
use flate2::write::ZlibEncoder;

const IMAGE_RAW_BYTES: usize = 2 * 1024 * 1024;
const DATASET_BYTES: usize = 15 * 1024 * 1024;
const ARCHIVE_STREAMS: usize = 8;
const ARCHIVE_STREAM_BYTES: usize = 1024 * 1024;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".claude/corpus/large-files"));

    if dir.join(".git").is_dir() {
        eprintln!(
            "{}/.git exists — remove the directory first to rebuild from scratch",
            dir.display()
        );
        std::process::exit(2);
    }
    fs::create_dir_all(&dir).expect("mkdir corpus root");
    git(&dir, &["init", "-q", "-b", "main"]);
    // Pin identity so the repo is byte-stable across machines.
    git(&dir, &["config", "user.name", "alt-corpus"]);
    git(&dir, &["config", "user.email", "alt-corpus@test"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);

    let images_dir = dir.join("images");
    let data_dir = dir.join("data");
    let archives_dir = dir.join("archives");
    let docs_dir = dir.join("docs");
    for d in [&images_dir, &data_dir, &archives_dir, &docs_dir] {
        fs::create_dir_all(d).expect("mkdir subdir");
    }

    // ---- C1: initial drop ----
    for (i, seed) in (1..=4_u32).map(|i| (i, 100 * i)).collect::<Vec<_>>() {
        write_image(&images_dir.join(format!("image{i:02}.png")), seed, 0);
    }
    for (i, seed) in (1..=4_u32).map(|i| (i, 1000 * i)).collect::<Vec<_>>() {
        write_dataset(&data_dir.join(format!("dataset{i:02}.dat")), seed);
    }
    write_archive(&archives_dir.join("sample01.bin"), 9999, 0);
    write_manifest(&docs_dir.join("manifest.json"), 1, "initial drop");
    commit(
        &dir,
        "C1: initial corpus drop (4 images, 4 datasets, 1 archive)",
    );

    // ---- C2: in-place pixel tweak on image01 ----
    write_image(&images_dir.join("image01.png"), 100, 1); // generation = 1
    write_manifest(&docs_dir.join("manifest.json"), 2, "image01 tweaked");
    commit(
        &dir,
        "C2: tweak image01 pixels (small change in zlib stream)",
    );

    // ---- C3: append to dataset01 ----
    append_dataset(&data_dir.join("dataset01.dat"), 1001);
    write_manifest(&docs_dir.join("manifest.json"), 3, "dataset01 appended");
    commit(
        &dir,
        "C3: append to dataset01 (lineage delta on growing blob)",
    );

    // ---- C4: mid-file modification of archive ----
    write_archive(&archives_dir.join("sample01.bin"), 9999, 1); // generation = 1
    write_manifest(&docs_dir.join("manifest.json"), 4, "archive mid-mod");
    commit(
        &dir,
        "C4: rewrite middle stream of sample01.bin (chunk dedup)",
    );

    // ---- C5: replace image04 entirely ----
    write_image(&images_dir.join("image04.png"), 100 * 4 + 9999, 0);
    write_manifest(&docs_dir.join("manifest.json"), 5, "image04 replaced");
    commit(
        &dir,
        "C5: replace image04 with a fresh seed (whole-blob churn)",
    );

    // Print a summary so the caller can sanity check size + commit count.
    let bytes = dir_bytes(&dir);
    eprintln!(
        "built {} ({:.1} MB across .git+working tree, 5 commits)",
        dir.display(),
        bytes as f64 / (1024.0 * 1024.0)
    );
}

fn write_image(path: &Path, seed: u32, generation: u32) {
    // "Image": a deflate stream of seeded pseudo-pixel bytes, wrapped in
    // a fake PNG-shaped envelope so the produced file contains a real
    // zlib stream (the prism's primary trigger). Not a parser-valid PNG —
    // B1's job is to exercise storage; B3 will add real PNG fixtures when
    // perceptual diff lands.
    let mut raw = vec![0u8; IMAGE_RAW_BYTES];
    fill_pixels(&mut raw, seed, generation);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(1));
    encoder.write_all(&raw).unwrap();
    let stream = encoder.finish().unwrap();

    let mut out = Vec::with_capacity(stream.len() + 32);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    out.extend_from_slice(&(stream.len() as u32).to_be_bytes());
    out.extend_from_slice(b"IDAT");
    out.extend_from_slice(&stream);
    out.extend_from_slice(b"IEND");
    write_file(path, &out);
}

fn fill_pixels(buf: &mut [u8], seed: u32, generation: u32) {
    // Gradient-with-noise pixels: highly structured so the deflate stream
    // varies smoothly under generation change (B3's perceptual diff
    // wants small bit-level diff for small visual diff). Lehmer LCG keeps
    // the generator deterministic without pulling in a crate.
    let mut s = seed.wrapping_add(generation.wrapping_mul(0xdead_beef));
    for (i, b) in buf.iter_mut().enumerate() {
        s = s.wrapping_mul(48271) % 0x7fff_ffff;
        let gradient = (i as u32 / 4096) as u8;
        let noise = (s & 0x0F) as u8;
        *b = gradient.wrapping_add(noise);
    }
}

fn write_dataset(path: &Path, seed: u32) {
    let mut buf = vec![0u8; DATASET_BYTES];
    fill_random(&mut buf, seed);
    write_file(path, &buf);
}

fn append_dataset(path: &Path, append_seed: u32) {
    let mut existing = fs::read(path).expect("read for append");
    let mut tail = vec![0u8; DATASET_BYTES / 4];
    fill_random(&mut tail, append_seed);
    existing.extend_from_slice(&tail);
    write_file(path, &existing);
}

fn fill_random(buf: &mut [u8], seed: u32) {
    // SplitMix-style PRNG, deterministic and fast — gives incompressible
    // bytes so the dataset behaves like a real model weight blob (CDC
    // stress case).
    let mut s: u64 = (seed as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for chunk in buf.chunks_mut(8) {
        s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        let bytes = z.to_le_bytes();
        let n = chunk.len();
        chunk.copy_from_slice(&bytes[..n]);
    }
}

fn write_archive(path: &Path, seed: u32, generation: u32) {
    // Container-shaped: header + N concatenated zlib streams + footer. The
    // generation parameter swaps in a *single* middle stream so a chunk
    // store with content-defined chunking dedups everything else.
    let mut out = Vec::new();
    out.extend_from_slice(b"ALTCORP1");
    out.extend_from_slice(&(ARCHIVE_STREAMS as u32).to_be_bytes());
    for stream_idx in 0..ARCHIVE_STREAMS {
        let stream_seed = seed.wrapping_add(stream_idx as u32 * 7);
        let stream_gen = if stream_idx == ARCHIVE_STREAMS / 2 {
            generation
        } else {
            0
        };
        let mut raw = vec![0u8; ARCHIVE_STREAM_BYTES];
        fill_pixels(&mut raw, stream_seed, stream_gen);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(1));
        encoder.write_all(&raw).unwrap();
        let stream = encoder.finish().unwrap();
        out.extend_from_slice(&(stream.len() as u32).to_be_bytes());
        out.extend_from_slice(&stream);
    }
    out.extend_from_slice(b"ENDCORP1");
    write_file(path, &out);
}

fn write_manifest(path: &Path, version: u32, note: &str) {
    let body =
        format!("{{\"version\":{version},\"note\":\"{note}\",\"corpus\":\"large-files\"}}\n");
    write_file(path, body.as_bytes());
}

fn write_file(path: &Path, data: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir parent");
    }
    fs::write(path, data).unwrap_or_else(|e| panic!("write {path:?}: {e}"));
}

fn commit(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty-message",
            "-m",
            message,
        ],
    );
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {dir:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                total += dir_bytes(&entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}
