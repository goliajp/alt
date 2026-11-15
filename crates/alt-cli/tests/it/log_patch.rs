//! `alt log -p` emits per-commit patches. Text files get a unified
//! diff (cross-checked against git); binary files stay compact — a
//! chunk-diff summary + a perceptual hint when the content is a kind we
//! recognise — so a binary-asset history doesn't blow up the terminal.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    crate::alt_command()
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
    let stream = alt_deflate::compress(&raw, 6, alt_deflate::Format::Zlib);
    chunk(&mut out, b"IDAT", &stream);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[test]
fn log_patch_emits_text_unified_diff() {
    let dir = alt_os::temp::tempdir().unwrap();
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
    let dir = alt_os::temp::tempdir().unwrap();
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
    let dir = alt_os::temp::tempdir().unwrap();
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

fn git_at(repo: &Path, date: u32, args: &[&str]) -> String {
    let stamp = format!("@{date} +0000");
    ok(Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .env("GIT_AUTHOR_DATE", &stamp)
        .env("GIT_COMMITTER_DATE", &stamp)
        .args(args)
        .output()
        .unwrap())
}

fn commit_all(repo: &Path, date: u32, message: &str) {
    git_at(repo, date, &["add", "-A"]);
    git_at(repo, date, &["commit", "-qm", message]);
}

fn source_file(sections: u32) -> String {
    let mut text = String::new();
    for s in 1..=sections {
        text.push_str(&format!("fn section_{s}() {{\n"));
        for l in 1..=6 {
            text.push_str(&format!("    step_{s}_{l}();\n"));
        }
        text.push_str("}\n\n");
    }
    text
}

/// A history with every kind of change a patch can show, built by git.
fn varied_history(src: &Path) {
    let put = |rel: &str, text: &str| {
        let p = src.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    git_at(src, 1000, &["init", "-q", "-b", "main", "."]);
    put("a.rs", &source_file(4));
    put("b.txt", "goes away\n");
    put("dir/c.rs", &source_file(3));
    put("script.sh", "#!/bin/sh\necho hi\n");
    put("kept.txt", "same\n");
    put("with space.txt", "one\n");
    put("caf\u{e9} \"q\".txt", "one\n");
    put("dir/tab\there.txt", "one\n");
    commit_all(src, 1000, "base");

    put(
        "a.rs",
        &source_file(4).replace("step_3_4();", "changed_3_4();"),
    );
    fs::remove_file(src.join("b.txt")).unwrap();
    put("new.txt", "brand new\nwithout a final newline");
    put("with space.txt", "two\n");
    put("caf\u{e9} \"q\".txt", "two\n");
    fs::rename(src.join("dir/tab\there.txt"), src.join("dir/tab moved.txt")).unwrap();
    commit_all(src, 1100, "edit, delete, add, names that need quoting");

    git_at(src, 1150, &["switch", "-q", "-c", "side"]);
    put("side.txt", "from the side\n");
    commit_all(src, 1150, "side work");
    git_at(src, 1200, &["switch", "-q", "main"]);

    fs::rename(src.join("dir/c.rs"), src.join("dir/renamed.rs")).unwrap();
    put(
        "dir/renamed.rs",
        &source_file(3).replace("step_2_2();", "renamed_2_2();"),
    );
    fs::create_dir(src.join("moved")).unwrap();
    fs::rename(src.join("kept.txt"), src.join("moved/kept.txt")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(src.join("script.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    commit_all(src, 1200, "rename with an edit, plain move, mode change");

    git_at(
        src,
        1300,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );

    fs::remove_file(src.join("new.txt")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.rs", src.join("new.txt")).unwrap();
    put("empty", "");
    put("a.rs", &source_file(4).replace("    step_1_1();\n", ""));
    commit_all(
        src,
        1400,
        "file becomes a link, empty file, edit at the top",
    );
}

#[test]
fn log_patch_matches_git_for_every_kind_of_change() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    varied_history(src);
    ok(alt(src, &["import", dst.to_str().unwrap()]));

    let ours = ok(alt(dst, &["log", "-p"]));
    let theirs = git_at(src, 0, &["log", "-p", "--pretty=oneline"]);
    assert_eq!(ours, theirs);
    // spot checks, so that the comparison is not two empty outputs
    assert!(ours.contains("similarity index"), "{ours}");
    assert!(ours.contains("--- a/with space.txt\t\n"), "{ours}");
    assert!(
        ours.contains("rename from \"dir/tab\\there.txt\"\n"),
        "{ours}"
    );
    assert!(
        ours.contains("diff --git \"a/caf\\303\\251 \\\"q\\\".txt\""),
        "{ours}"
    );
    assert!(ours.contains("rename from kept.txt\nrename to moved/kept.txt\n"));
    assert!(ours.contains("@@ fn section_3() {"), "{ours}");
    assert!(ours.contains("old mode 100644\nnew mode 100755\n"));
    assert!(ours.contains("deleted file mode 100644\n"));
    assert!(ours.contains("new file mode 120000\n"));

    let raw = ok(alt(dst, &["log", "-p", "--pretty", "raw"]));
    assert_eq!(raw, git_at(src, 0, &["log", "-p", "--pretty=raw"]));
}

#[test]
fn show_prints_no_patch_for_a_merge_that_only_took_sides() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    varied_history(src);
    ok(alt(src, &["import", dst.to_str().unwrap()]));
    let merge = git_at(src, 0, &["rev-parse", "HEAD~1"]);
    let shown = ok(alt(dst, &["show", merge.trim()]));
    assert!(shown.contains("merge side"), "{shown}");
    assert!(!shown.contains("diff --"), "{shown}");
    assert_eq!(
        shown,
        git_at(src, 0, &["show", "--pretty=raw", merge.trim()])
    );
    // a commit with one parent still shows its patch, set apart by a blank line
    let plain = ok(alt(dst, &["show", "HEAD"]));
    assert_eq!(plain, git_at(src, 0, &["show", "--pretty=raw", "HEAD"]));
}

/// A merge whose result is written by hand: `main` and `side` both changed
/// `code.rs`, and the merge commit holds whatever `resolve` leaves in the
/// work tree on top of main's side.
fn hand_merged(src: &Path, resolve: &dyn Fn(&Path, &str, &str)) {
    git_at(src, 1000, &["init", "-q", "-b", "main", "."]);
    let base = source_file(6);
    fs::write(src.join("code.rs"), &base).unwrap();
    fs::write(src.join("both-drop.txt"), "dropped by the merge\n").unwrap();
    fs::write(src.join("side-only.txt"), "one\ntwo\nthree\n").unwrap();
    fs::write(src.join("blob.bin"), b"\0base").unwrap();
    commit_all(src, 1000, "base");
    git_at(src, 1100, &["switch", "-q", "-c", "side"]);
    let side = base
        .replace("step_2_3();", "side_2_3();")
        .replace("    step_5_1();\n", "")
        .replace("fn section_6() {", "fn section_6() { // side");
    fs::write(src.join("code.rs"), &side).unwrap();
    fs::write(src.join("side-only.txt"), "one\nTWO\nthree\n").unwrap();
    fs::write(src.join("both-drop.txt"), "changed on side\n").unwrap();
    fs::write(src.join("blob.bin"), b"\0side").unwrap();
    commit_all(src, 1100, "side");
    git_at(src, 1200, &["switch", "-q", "main"]);
    let main = base
        .replace("step_2_3();", "main_2_3();")
        .replace("step_4_4();", "main_4_4();");
    fs::write(src.join("code.rs"), &main).unwrap();
    fs::write(src.join("both-drop.txt"), "changed on main\n").unwrap();
    fs::write(src.join("blob.bin"), b"\0main").unwrap();
    commit_all(src, 1200, "main");
    git_at(
        src,
        1300,
        &["merge", "-q", "-s", "ours", "--no-commit", "side"],
    );
    resolve(src, &main, &side);
    commit_all(src, 1300, "merge by hand");
}

#[test]
fn show_prints_the_combined_diff_of_a_merge_as_git_does() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    hand_merged(src, &|root, main, side| {
        // both versions of the contested line are replaced, side's removal
        // and comment are taken, and one line is the merge's own
        let merged = main
            .replace("main_2_3();", "merged_2_3();")
            .replace("    step_5_1();\n", "")
            .replace("fn section_6() {", "fn section_6() { // side")
            .replace("step_1_2();", "only_the_merge_1_2();");
        assert_ne!(merged, side);
        fs::write(root.join("code.rs"), merged).unwrap();
        fs::write(root.join("side-only.txt"), "one\nTWO\nthree\n").unwrap();
        fs::remove_file(root.join("both-drop.txt")).unwrap();
        fs::write(root.join("blob.bin"), b"\0merged").unwrap();
        fs::write(root.join("born in the merge.txt"), "new\nfile\n").unwrap();
    });
    ok(alt(src, &["import", dst.to_str().unwrap()]));

    let ours = ok(alt(dst, &["show", "HEAD"]));
    assert_eq!(ours, git_at(src, 0, &["show", "--pretty=raw", "HEAD"]));
    // what must be there, so that two empty outputs do not pass
    assert!(ours.contains("diff --cc code.rs\n"), "{ours}");
    assert!(ours.contains("\n@@@ -"), "{ours}");
    assert!(ours.contains("\n++    merged_2_3();\n"), "{ours}");
    assert!(ours.contains("\n -    side_2_3();\n"), "{ours}");
    assert!(ours.contains("deleted file mode 100644,100644\n"), "{ours}");
    assert!(ours.contains("diff --cc blob.bin\n"), "{ours}");
    assert!(ours.contains("Binary files differ\n"), "{ours}");
    assert!(
        ours.contains("diff --cc \"born in the merge.txt\"\n")
            || ours.contains("diff --cc born in the merge.txt\n")
    );
    assert!(
        !ours.contains("side-only.txt"),
        "taken whole from one side: {ours}"
    );

    // the binary file's summary line is alt's own; the rest is git's
    let log = |args: &[&str]| -> String {
        let text = ok(alt(dst, args));
        let kept = text.lines().filter(|l| !l.starts_with("chunks: "));
        kept.map(|l| format!("{l}\n")).collect()
    };
    assert_eq!(
        log(&["log", "-p", "--cc"]),
        git_at(src, 0, &["log", "-p", "--cc", "--pretty=oneline"])
    );
    // without --cc a merge has no patch in the log
    assert_eq!(
        log(&["log", "-p"]),
        git_at(src, 0, &["log", "-p", "--pretty=oneline"])
    );
}

#[test]
fn combined_diff_of_a_merge_of_three_branches_matches_git() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    git_at(src, 1000, &["init", "-q", "-b", "main", "."]);
    let base = source_file(5);
    fs::write(src.join("code.rs"), &base).unwrap();
    commit_all(src, 1000, "base");
    for (date, branch, from, to) in [
        (1100, "one", "step_1_3();", "one_1_3();"),
        (1200, "two", "step_3_3();", "two_3_3();"),
    ] {
        git_at(src, date, &["switch", "-q", "-c", branch, "main"]);
        fs::write(src.join("code.rs"), base.replace(from, to)).unwrap();
        commit_all(src, date, branch);
    }
    git_at(src, 1300, &["switch", "-q", "main"]);
    fs::write(
        src.join("code.rs"),
        base.replace("step_5_3();", "main_5_3();"),
    )
    .unwrap();
    commit_all(src, 1300, "main");
    git_at(
        src,
        1400,
        &["merge", "-q", "-s", "ours", "--no-commit", "one", "two"],
    );
    let merged = base
        .replace("step_1_3();", "one_1_3();")
        .replace("step_3_3();", "octopus_3_3();")
        .replace("step_5_3();", "main_5_3();")
        .replace("    step_4_6();\n", "");
    fs::write(src.join("code.rs"), merged).unwrap();
    commit_all(src, 1400, "three way");
    ok(alt(src, &["import", dst.to_str().unwrap()]));
    let ours = ok(alt(dst, &["show", "HEAD"]));
    assert_eq!(ours, git_at(src, 0, &["show", "--pretty=raw", "HEAD"]));
    assert!(ours.contains("\n@@@@ -"), "{ours}");
    assert!(ours.contains("\n+++    octopus_3_3();\n"), "{ours}");
}

#[test]
fn log_patch_keeps_a_directory_apart_from_files_named_like_it() {
    // in a tree `notes` (a directory) sorts after `notes-old.txt` and
    // `notes.txt`; taking it for a plain name would report its files as
    // removed and added again
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    git_at(src, 1000, &["init", "-q", "-b", "main", "."]);
    fs::create_dir(src.join("notes")).unwrap();
    fs::write(src.join("notes/a.txt"), "kept as it is\n").unwrap();
    fs::write(src.join("m.txt"), "m\n").unwrap();
    commit_all(src, 1000, "base");
    fs::write(src.join("notes-old.txt"), "beside the directory\n").unwrap();
    fs::write(src.join("notes.txt"), "also beside it\n").unwrap();
    commit_all(src, 1100, "files named like the directory");
    ok(alt(src, &["import", dst.to_str().unwrap()]));

    let ours = ok(alt(dst, &["log", "-p", "-n", "1"]));
    assert_eq!(
        ours,
        git_at(src, 0, &["log", "-p", "--pretty=oneline", "-n", "1"])
    );
    assert!(!ours.contains("notes/a.txt"), "{ours}");
}
