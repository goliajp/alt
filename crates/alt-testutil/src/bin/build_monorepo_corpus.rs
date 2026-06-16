//! M8-A1 monorepo synth corpus generator.
//!
//! Builds a deterministic typical-webdev-monorepo-shaped git repo at
//! `<dir>`: 200 packages, each with a small manifest + README + ~250
//! source files in nested directories (depth 1-5), then 8 000 commits
//! that touch 1-3 files at random (90%) or fan out across 2-3 packages
//! (10%) — the modify pattern A7 monorepo bench needs to stress
//! status / commit / diff / log on a realistic working tree.
//!
//! Reproducible (same seed → same bytes), so a corpus rebuild diff is a
//! tooling regression. Invoked via `cargo run -p alt-testutil --bin
//! build-monorepo-corpus -- <dir>` or `scripts/build-monorepo-corpus.sh`
//! that defaults to `.claude/corpus/monorepo`.
//!
//! Sizes are tunable via env vars (handy when iterating on the bench
//! shape): `ALT_MONOREPO_PACKAGES` (default 200), `ALT_MONOREPO_FILES_PER_PKG`
//! (default 250), `ALT_MONOREPO_COMMITS` (default 8000).
//!
//! Uses git's `fast-import` plumbing rather than per-commit `git add` +
//! `git commit` — at 50k files an index rewrite per commit drops to
//! ~2 s/commit (8 000 commits ≈ 4-5 h), while fast-import streams the
//! whole history in one process and finishes in ~30 s. The output is a
//! standard git repo that the rest of alt's tooling can read normally.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_PACKAGES: usize = 200;
const DEFAULT_FILES_PER_PKG: usize = 250;
const DEFAULT_COMMITS: usize = 8000;
const LINES_PER_FILE: usize = 60;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".claude/corpus/monorepo"));
    if dir.join(".git").is_dir() {
        eprintln!(
            "{}/.git exists — remove the directory first to rebuild",
            dir.display()
        );
        std::process::exit(2);
    }

    let packages = env_usize("ALT_MONOREPO_PACKAGES", DEFAULT_PACKAGES);
    let files_per_pkg = env_usize("ALT_MONOREPO_FILES_PER_PKG", DEFAULT_FILES_PER_PKG);
    let commits = env_usize("ALT_MONOREPO_COMMITS", DEFAULT_COMMITS);

    fs::create_dir_all(&dir).expect("mkdir corpus root");
    git_ok(&dir, &["init", "-q", "-b", "main"]);
    git_ok(&dir, &["config", "user.name", "monorepo-bot"]);
    git_ok(&dir, &["config", "user.email", "monorepo@test"]);
    git_ok(&dir, &["config", "commit.gpgsign", "false"]);
    git_ok(&dir, &["config", "core.fsync", "none"]);
    git_ok(&dir, &["config", "gc.auto", "0"]);

    let layout = layout_paths(packages, files_per_pkg);
    eprintln!(
        "streaming {} packages × ~{} files = {} files initial + {} commits...",
        packages,
        files_per_pkg,
        layout.len(),
        commits
    );

    let t0 = std::time::Instant::now();
    // Spawn fast-import once and feed it the full history. The protocol
    // is line-oriented per command; data payloads are length-prefixed.
    let mut child = Command::new("git")
        .current_dir(&dir)
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn git fast-import");
    let stdin = child.stdin.take().expect("fast-import stdin");
    let mut w = BufWriter::with_capacity(1 << 20, stdin);

    // Use marks: :1..N for initial blobs (slot → mark), then a running
    // counter for incremental blobs. Commits get their own mark space.
    let initial_blobs_base: usize = 1;
    let commit_marks_base: usize = initial_blobs_base + layout.len();
    let mut next_blob_mark: usize = commit_marks_base + commits + 2;

    // --- initial blobs ---
    for (idx, path) in layout.iter().enumerate() {
        let mark = initial_blobs_base + idx;
        let body = generate_file(path, idx, 0);
        write_blob(&mut w, mark, &body);
    }
    eprintln!("  wrote {} initial blobs", layout.len());

    // --- initial commit: every path → mark ---
    let initial_commit_mark = commit_marks_base;
    write_commit_header(
        &mut w,
        initial_commit_mark,
        "initial monorepo drop",
        0,
        None,
    );
    for (idx, path) in layout.iter().enumerate() {
        writeln!(
            w,
            "M 100644 :{} {}",
            initial_blobs_base + idx,
            fast_import_path(path)
        )
        .unwrap();
    }
    writeln!(w).unwrap();

    // --- incremental commits ---
    let mut prev = initial_commit_mark;
    let mut next_log = std::time::Instant::now();
    for ci in 1..=commits {
        let touched = pick_touched(&layout, ci);
        let mut blob_marks: Vec<(usize, usize)> = Vec::with_capacity(touched.len());
        for (slot, generation) in &touched {
            let path = &layout[*slot];
            let body = generate_file(path, *slot, *generation);
            let mark = next_blob_mark;
            next_blob_mark += 1;
            write_blob(&mut w, mark, &body);
            blob_marks.push((*slot, mark));
        }
        let commit_mark = commit_marks_base + ci;
        write_commit_header(
            &mut w,
            commit_mark,
            &format!("commit {ci}"),
            ci as i64,
            Some(prev),
        );
        for (slot, mark) in &blob_marks {
            writeln!(w, "M 100644 :{} {}", mark, fast_import_path(&layout[*slot])).unwrap();
        }
        writeln!(w).unwrap();
        prev = commit_mark;
        if next_log.elapsed().as_secs() >= 10 {
            eprintln!(
                "  ... {ci}/{} commits streamed, elapsed {:.1}s",
                commits,
                t0.elapsed().as_secs_f64()
            );
            next_log = std::time::Instant::now();
        }
    }

    // Point main at the last commit.
    writeln!(w, "reset refs/heads/main").unwrap();
    writeln!(w, "from :{prev}").unwrap();
    writeln!(w, "done").unwrap();
    drop(w);

    let status = child.wait().expect("wait fast-import");
    assert!(status.success(), "fast-import exited {status:?}");
    eprintln!("  fast-import done in {:.1}s", t0.elapsed().as_secs_f64());

    // Recreate the working tree and index from the imported main so that
    // tools like `alt import` (which reads .git/objects) and `git status`
    // see a normal repo, not a bare-ish one.
    let t1 = std::time::Instant::now();
    git_ok(&dir, &["reset", "--hard", "main"]);
    eprintln!("  reset --hard main in {:.1}s", t1.elapsed().as_secs_f64());

    let bytes = dir_bytes(&dir);
    eprintln!(
        "built {} ({:.1} MB across .git+working tree, {} commits, {} files)",
        dir.display(),
        bytes as f64 / (1024.0 * 1024.0),
        commits + 1,
        layout.len(),
    );
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

fn write_blob<W: Write>(w: &mut W, mark: usize, body: &[u8]) {
    writeln!(w, "blob").unwrap();
    writeln!(w, "mark :{mark}").unwrap();
    writeln!(w, "data {}", body.len()).unwrap();
    w.write_all(body).unwrap();
    writeln!(w).unwrap();
}

fn write_commit_header<W: Write>(
    w: &mut W,
    mark: usize,
    message: &str,
    ts_offset: i64,
    parent_mark: Option<usize>,
) {
    let ts = 1_700_000_000_i64 + ts_offset;
    writeln!(w, "commit refs/heads/main").unwrap();
    writeln!(w, "mark :{mark}").unwrap();
    writeln!(w, "author monorepo-bot <monorepo@test> {ts} +0000").unwrap();
    writeln!(w, "committer monorepo-bot <monorepo@test> {ts} +0000").unwrap();
    writeln!(w, "data {}", message.len()).unwrap();
    w.write_all(message.as_bytes()).unwrap();
    writeln!(w).unwrap();
    if let Some(p) = parent_mark {
        writeln!(w, "from :{p}").unwrap();
    }
}

/// fast-import wants paths as raw bytes after `M <mode> :<mark> ` — quote
/// when they contain spaces or special chars. Our layout uses only
/// `[a-zA-Z0-9_/.\-]` so the simple unquoted form is always safe.
fn fast_import_path(path: &str) -> &str {
    path
}

fn layout_paths(packages: usize, files_per_pkg: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(packages * (files_per_pkg + 2));
    for p in 0..packages {
        out.push(format!("pkg/p{p:04}/Cargo.toml"));
        out.push(format!("pkg/p{p:04}/README.md"));
        for f in 0..files_per_pkg {
            let depth = (f % 5) + 1;
            let mut subdirs = String::new();
            for d in 0..depth {
                subdirs.push_str(&format!("m{:02}/", (f / (d + 1)) % 16));
            }
            out.push(format!("pkg/p{p:04}/src/{subdirs}file{f:04}.rs"));
        }
    }
    out
}

fn generate_file(path: &str, slot: usize, generation: u32) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&format!("// generated for {path}\n"));
    out.push_str(&format!("// slot={slot}, generation={generation}\n\n"));
    let mut s = (slot as u64)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(generation as u64 * 0xbf58_476d_1ce4_e5b9);
    for line in 0..LINES_PER_FILE {
        s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        match z % 4 {
            0 => out.push_str(&format!(
                "fn op{:04}(x: u32) -> u32 {{ x.wrapping_add(0x{:08x}) }}\n",
                line,
                (z & 0xffff_ffff) as u32
            )),
            1 => out.push_str(&format!("struct S{line:04} {{ a: u64, b: u64 }}\n")),
            2 => out.push_str(&format!(
                "const K{line:04}: u32 = 0x{:08x};\n",
                (z >> 16) as u32
            )),
            _ => out.push_str(&format!(
                "/// doc line {line} hash=0x{:x}\n",
                (z >> 8) & 0xffff_ffff_ffff
            )),
        }
    }
    out.into_bytes()
}

fn pick_touched(layout: &[String], commit_idx: usize) -> Vec<(usize, u32)> {
    let mut s = (commit_idx as u64).wrapping_mul(0x94d0_49bb_1331_11eb);
    let pump = |s: &mut u64| -> u64 {
        *s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };

    let big = pump(&mut s) % 10 == 0; // 10% cross-cutting
    let mut out = Vec::new();
    if big {
        let nfiles = 5 + (pump(&mut s) % 16) as usize;
        for _ in 0..nfiles {
            let slot = (pump(&mut s) as usize) % layout.len();
            let generation = ((commit_idx + slot) % 1024) as u32;
            out.push((slot, generation));
        }
    } else {
        let nfiles = 1 + (pump(&mut s) % 3) as usize;
        let anchor = (pump(&mut s) as usize) % layout.len();
        for k in 0..nfiles {
            let slot = (anchor + k * 7) % layout.len();
            let generation = ((commit_idx + slot) % 1024) as u32;
            out.push((slot, generation));
        }
    }
    out.sort();
    out.dedup_by_key(|(s, _)| *s);
    out
}

fn git_ok(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
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
